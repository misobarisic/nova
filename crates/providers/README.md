# Content providers

Nova bundles AniKoto and the optional Supernova JavaScript provider. AniKoto appears in Settings → Addons
and in the series catalog/search flows. The `nova-provider://anikoto` transport
adapts its normalized output to the app's existing Stremio response parser, so
library entries, episode progress, metadata caches, downloads, and sync use the
same paths as other sources. The addon can be disabled or removed; it is
registered once per installation (`providers:bundled:v1`). Paste the transport
URL into Addons to reinstall it.

The normal Configure gear opens a sheet with independent Streams and Metadata
switches. Metadata includes catalog browsing and search; disabling it preserves
internal source lookups needed to resolve streams against another addon’s IDs.
Both switches default on for older installs. With both off the addon remains
installed but advertises no capabilities or catalogs. Choices live in the
existing addon JSON record and sync with that addon; source refreshes preserve
them and the transport URL remains unchanged. External Stremio configuration
continues to use the addon’s own website and install URL.

The Yuzono Android extensions contain Kotlin code for Aniyomi's APIs. Adding a
source here means porting its requests and parsing to Nova's JavaScript API.
This version loads trusted source files bundled at compile time.

## Supernova

`nova-provider://supernova` is installed **disabled** once per installation
(`providers:supernova:v1`). Enable it in Addons and use its normal Configure
gear to select sources you may use. Nyaa, AnimeTosho and APIBay are initially
off. Its stream-only manifest adds neither catalogs nor metadata and never
changes a title's library, tracking or episode identity.

Discovery runs on-device in QuickJS. Nyaa and AnimeTosho adapters port Comet's
broad title queries and request/parsing approach: unfiltered Nyaa HTML and
AnimeTosho Torznab XML, with the main-site JSON feed as a fallback. APIBay
supplies movie/TV JSON. Each source has at most two query variants and two pages
per query; native GET batches run at most three requests concurrently. Three-second
request limits and optional-stage deadlines preserve partial results within a
20-second lookup budget. Up to 150 ranked hashes reach TorBox availability checks
in three concurrent batches; the display remains capped at 50 rows.
Search metadata is cached locally for a day; partial failures and empty results
get a short retry TTL, and empty failed searches are not cached. Source access
can change or fail independently; no anti-bot bypass or background catalog exists.

Matching requires the title and movie year/confirmed IMDb ID, or a compatible
series inventory. Canonical title parsing keeps numeric sequels distinct and
separates title numbers from episode ranges. Season packs may reach inspection;
playback still requires exactly one file proving the requested episode. Anime
absolute numbering requires Nova's verified alignment. Wrong episodes, samples,
ambiguous videos and unsupported hashes are omitted. Before emitting P2P, up to
six unverified candidates per lookup fetch bounded metainfo; subsequent cache
hits retry remaining files without extending the original discovery expiry.
`torrent_metadata.rs` hashes the original v1 info dictionary and returns original
file indexes. JS verifies the hash and selects one matching video; bare episode
filenames also require a verified torrent root/container identifying the series.
It never asks the playback engine to guess the largest file. Conservative
matching may omit valid releases, especially differently titled editions and
later-season releases containing a year different from the series metadata.
Seeder counts are upstream reports. Reused discovery rows explicitly identify
cached discovery and unverified peer availability; TorBox availability is still
checked live independently of that cache.

Direct P2P uses the existing torrent engine and its P2P setting. The optional
TorBox preferred/cached-only modes require a configured key. Lookup only checks
availability; it never creates a torrent. A selected TorBox row rechecks current
settings/cache, reuses a verified account torrent or creates with
`add_only_if_cached=true` and `seed=3`, verifies the unique video against the
account file list, then requests a fresh link. TorBox file IDs are separate from
P2P indexes. Missing keys, uncached or ambiguous files, account errors and rate
limits never silently start an uncached transfer or substitute another video.
The existing playback token, addon revision and settings generation reject
stale completions after Back, suspend, replacement or configuration edits.

The API key is an ordinary persisted and synced preference, masked in the
sheet. TorBox's documented `requestdl` token parameter is used only internally;
request URLs, credentials and expiring playback URLs never enter discovery
records, resolver tokens or logs. Resolve errors shown in Nova use fixed text.
TorBox also returns credential-bearing CDN links. A manifest-declared exception
permits those links only at the final resolver boundary, for the declared HTTPS
CDN policy, path and matching credential parameter. Supernova currently opts
into `domains: ["*"]` for HTTP and final playback, allowing all public domains
as requested. Other providers retain their existing domain allowlists. Nova
passes credential-bearing links to in-app playback
only; external-player preferences and Android external fallbacks do not receive
them. The link is retained in memory for playback/recovery and never saved or
synced. Other stream fields and ordinary lookup rows still reject credentials.
No live credentials belong in fixtures; ignored live tests read a temporary
key file and prohibit POST requests.

`plugin_settings` sync uses independent `plugin/field` records and tombstones,
retaining unknown plugin IDs/fields. Operation snapshots include only fields
declared by the installed schema, so large future settings do not prevent older
providers from running. Disable/removal preserves settings.
The gear offers a separate confirmed **Remove configuration everywhere**
action and **Clear plugin cache**. Cache JSON lives locally at `plugin_cache:v1`
in the existing database, with a 20 MiB logical budget, 1024 records and a 1 MiB
entry limit; physical database allocation need not immediately shrink. Cache
clearing preserves preferences, images and torrent downloads.
Successful clearing invalidates in-flight cache operations, preventing old
searches from repopulating it. Startup restores retained unfamiliar sync records
before seeding settings, including unknown fields and deletion tombstones.

`build.rs` concatenates the trusted `plugins/supernova/{core,matching,index}.js`
and source/debrid modules. `supernova_tests.rs` uses real QuickJS/parsers with
deterministic HTTP fixtures; `supernova_live_tests.rs` is explicitly ignored
in normal test runs. See the bundled `NOTICE.md` for source/protocol references.

## Generic plugin services

Normalized streams choose exactly one direct URL, v1 hash/file index or owned
`resolver: { provider_id, token }`. The Stremio adapter maps resolver selections
to the Nova-only `novaResolver` extra; ordinary remote addons cannot use it to
invoke another provider. Movie contextual lookups omit season/episode; series
lookups require both. Resolver operations receive a new immutable settings
snapshot and return a validated direct stream.

`configuration` in the plugin manifest describes boolean, string, secret,
integer and choice controls with typed defaults/options/bounds. Nova renders
the schema using its existing controls, saves text with Enter or Save, and
validates each explicit update. Unknown persisted fields survive unchanged.

```js
JSON.parse(nova.http.request(JSON.stringify({
  method: "GET", url: "https://allowed.example/feed", timeoutMs: 1500,
}))); // status + body (text), or an independent error
// responseType: "binary" returns bodyBase64 instead, for torrent metainfo.
JSON.parse(nova.http.batch(JSON.stringify([{method: "GET", url: "https://allowed.example/feed"}])));
// POST requires permissions.httpPost, with body.type text/json/urlencoded/multipart.
JSON.parse(nova.xml.select(xml, "item", JSON.stringify({title: {selector: "title"}})));
// value: "attributes" returns a JSON array of matching sibling attribute maps.
// It keeps Torznab name/value fields associated with their owning item.
JSON.parse(nova.torrent.inspect(base64Metainfo)); // verified infoHash + original files/indexes
JSON.parse(nova.settings.get("quality.preferred")); // frozen operation snapshot
nova.settings.set("quality.preferred", JSON.stringify("1080p"));
nova.settings.remove("quality.preferred");
nova.cache.set("lookup:v1:opaque-digest", JSON.stringify(result), 86400);
JSON.parse(nova.cache.get("lookup:v1:opaque-digest") || "null");
nova.cache.remove("lookup:v1:opaque-digest");
```

HTTP revalidates and pins public DNS for each target. Bearer requests and POST
cannot redirect to another origin. Methods/bodies, aggregate bytes and requests,
DNS/redirect/read time, XML entities/depth/items and torrent nodes/paths/pieces
are bounded. Optional cache failures do not fail searches; settings edits are
acknowledged only after durable persistence. Cache reads/writes compare the
captured generation atomically; remote projection and local edits share the
same service mutation lock.

## Files and interface

- `src/models.rs` / `src/ids.rs`: normalized provider models and typed external IDs.
- `src/registry.rs`: bundled provider definitions, generic JS provider adapter,
  source ownership, and `nova-provider://<provider-id>` protocol dispatch.
- `src/runtime.rs` / `src/host.rs`: QuickJS capabilities, HTTP policy, DNS
  validation/pinning, and bounded shared session storage.
- `src/metadata.rs`: installed-addon metadata broker, ID connections, enrichment,
  provenance, and session caching. Transport is injected by `nova-media`.
- `src/sequence.rs` / `src/matching.rs`: provider-neutral candidate matching and
  forward/reverse episode alignment.
- `src/stremio.rs`: Stremio normalization, including typed identity claims.
- `plugins/anikoto/`: site scraping, source-family discovery, playback extraction,
  permission manifest, addon manifest, and notice.

Invoke providers on background workers. A source exposes
`globalThis.novaProvider.handle(request)` and returns a JSON-compatible value:

```js
{ op: "catalog", catalogId: "anikoto.popular", extra: { skip: "0" } }
{ op: "catalog", catalogId: "anikoto.search", extra: { search: "title", skip: "0" } }
{ op: "details", sourceId: "opaque-source-id" }
{ op: "streams", episodeId: "ep:opaque-episode-id" }
{ op: "lookupStreams", lookup: { mediaId: "tt5095466", mediaType: "series", title: "The Asterisk War", year: "2015", season: 1, episode: 13, absoluteEpisode: 13, seasonEpisodeCount: 24, seriesEpisodeCount: 24, episodeTitle: "Divine Revelations", released: "2016-04-02" } }
```

Catalogs return `MediaItem[]`; details return `{ media: MediaItem, episodes:
Episode[], enrichment?: EnrichmentResult }`; streams return `ProviderStream[]`.
Normalized models use snake case, while host-service requests use camel case.
Persisted provider IDs remain `provider_id:source_id`. AniKoto episodes encode
show path and native episode number; expiring server tokens are fetched only
at playback time. Descriptors/catalogs come from the addon manifest.
`capabilities.contextualStreams` enables the contextual `resolve/series/...`
transport, rather than app code checking a particular provider name.
`capabilities.configurableCapabilities` opts a bundled source into the local
configuration sheet. It must implement `manifest` with the unfiltered source
manifest and `config: { streams, metadata }`, and gate exposed actions using
that config. The bridge also gates protocol requests before HTTP and injects
config into every JS action; internal matching calls remain available.

### Rust metadata services called from JS

Grant `permissions.addonMetadata: true` to use the following capability. JS
supplies normalized source information; Rust handles matching, enrichment,
network budgets, caching, and ambiguity. Scripts do not receive account tokens
or configured addon URLs; provenance uses opaque addon IDs.

```js
const native = { media, episodes };
if (nova.metadata.available()) {
  nova.metadata.checkpoint(JSON.stringify(native));
  const enriched = JSON.parse(nova.metadata.enrich(JSON.stringify({
    details: native,
    sourceSequences,
    query: familyTitle,
    targets: ["imdb", "tmdb:tv", "mal:anime", "anilist:anime"],
  })));
  if (!enriched.error) {
    return { ...native, media: enriched.details.media, enrichment: enriched };
  }
}
return native;
```

`SourceSequence` contains `mediaId`, title/aliases/year, optional season/part and
`declaredCount`, and available episodes `{ id, number, title, released }`.
Episode IDs are stable native IDs including the provider prefix. Include the
opened entry even if it is absent from search results. AniKoto retains site
parsing and related-entry discovery in JS and reuses the already-fetched rows.

`EnrichmentResult` contains `status` (`confirmed`, `ambiguous`, or `missing`),
`inventoryRevision`, `details`, show `connections`, and `episodeMetadata` keyed by native episode ID.
A connection records an opaque `addonId`, `mediaId`, typed IDs, and confirmation
basis. Conflicting identifiers remain typed claims with a conflicting basis;
they do not create outbound stream aliases. `details.episodes` retains native
playback numbering. The generic protocol adapter applies confirmed display
metadata and aliases separately without replacing stable IDs.

For the other direction, plugins call:

```js
const match = JSON.parse(nova.metadata.resolveEpisode(JSON.stringify({
  target: request.lookup,
  sourceSequences,
})));
if (match.status !== "confirmed") return [];
// Refresh tokens using match.sourceEpisodeId, not a canonical playback ID.
```

The result includes `sourceEpisodeId`, `sourceMediaId`, and native `number`.
The same Rust engine supports separate seasons, split/merged cours, unequal
part lengths, continuous numbering, ongoing declared counts, and unique title
anchors. Explicit season/cour labels before a subtitle (for example,
`Show Season 2: A New Arc`) retain their season identity while searching the
parent series name. The later entry may start after the parent series; it must
still pass episode alignment and identifier/conflict checks. Unlabeled titles
keep the strict year check. Local mapping cache keys include the matcher version
so parser improvements replace obsolete results. A standalone later-season
entry may number its own episodes as season 1; its explicit title label supplies
alignment context only when complete regular counts prove a single season and
absolute numbering starts at episode 1. Native IDs and metadata numbering stay
unchanged, and only available source episodes map. It rejects missing parts, numbering gaps, duplicate editions,
conflicting years/titles, specials, and ambiguous mappings. Shortened series
names need two globally unique positional episode anchors with equal season
counts and no conflicting anchors to confirm the remaining positions.

### Installed addons as the enrichment network

Rust resolves explicit external IDs first, then searches remaining enabled,
available addon catalogs declaring search support and fetches candidate details
through ordinary catalog/meta endpoints. Already connected addons skip redundant
title searches. Confirmed
identifiers can find further compatible metadata addons, connecting IMDb,
TMDB, MAL, and AniList when the installed sources provide those IDs. `targets`
expresses the desired connection namespaces; it does not synthesize IDs or
promise that an installed addon provides every namespace. Cinemeta is an
ordinary installed source, with no hidden fallback when disabled or removed.

The app publishes an immutable addon snapshot on inventory changes. Enrichment
runs on fetch workers with a five-second deadline, up to 32 requests, 100
previews per search response, six detail candidates, and 2 MiB responses.
Visited requests and a nested-operation guard prevent lookup cycles; nested
JS providers receive a deadline bounded by the remaining metadata budget.
Optional failures preserve native metadata and playback. `checkpoint` saves a
bounded native details response before optional JS discovery so a subsequent
timeout can return that response without resuming interrupted JS. It is available
only during a details operation and validates provider ownership.

Host metadata caching allows 256 KiB entries under the same 2 MiB session
storage cap used by JS; JS storage keeps its 16 KiB entry cap. Successful results
last 30 days and persist across restarts through the app-injected
`MetadataCache` backend (128 entries, 8 MiB total, 256 KiB each). Failed refreshes
retain confirmed mappings for unchanged inputs; explicit conflicts invalidate
them. Missing/ambiguous results stay session-only for 30 seconds. Cache keys include
source sequence/availability, requested namespaces, and the addon inventory
revision. Manifest objects are canonicalized so identical inventories keep
their revision across restarts. Oversized results remain usable without caching.
Field-selection implementation versions also participate in the fingerprint.
Empty strings do not block fallback. Display fields are selected after conflict
filtering, and another accepted provider may fill missing episode art/text.
Duplicate episode aliases retain per-addon provenance until that filtering,
then collapse to one outbound alias. Native IDs and availability stay intact.
Official Cinemeta candidates also receive bounded live-endpoint thumbnail
repair before mapping, using the remaining enrichment deadline. It requires
one-to-one normalized-title matches within the same IMDb series and matching
special/regular classification, preserving native episode identities. Unique
titles tolerate missing/differing dates; repeated titles require a unique air-date
match within one calendar day, including month/year boundaries. The
matcher version is part of cache keys so matching fixes invalidate obsolete results.

Ordinary addon details also use this service. Bundled and standalone entries
retain native availability. Main series may add confirmed regular episode
coverage from another enabled addon identifying the same IMDb/TMDB TV parent.
`metadata/coverage.rs` uses ID/alias/alignment evidence to reconcile partial
seasons and alternative numbering without duplicate cards. Existing saved
episode IDs and numbering are retained when sources change or return shorter
lists; refreshed source IDs become owned stream aliases. The app reuses its
existing episode cache as the baseline for Detail, Home and prefetch. Confirmed episode metadata supplies meaningful titles, dates,
artwork, descriptions, and canonical season/episode numbers. Aliases are stored
as `novaStreamIds` with `novaConnections` and `novaMetadataRevision`; typed show
claims use `novaExternalIds`. The inventory revision prevents obsolete cached
aliases from routing after addons change. Source ownership restricts private
IDs to their owner; foreign requests use confirmed aliases accepted by the
recipient manifest. Non-global aliases additionally require matching provenance.
Library history, downloads, and sync continue using original native IDs.
Fresh aliases refresh cached pickers and restart an open stream search; search
generations reject stale replies, while active playback is left alone.

Settings → Addons searches installed names, descriptions and URLs and filters
All / Searchable catalogs / Metadata / Streams. Disabled addons remain visible
there. Contextual providers show “Streams for other sources.” Additional sources
are installed by URL; this version does not integrate an external directory.

## Injected JavaScript API

| API | Result |
| --- | --- |
| `nova.http.get(url, JSON.stringify(headers))` | JSON string with `status`, `url`, `contentType`, `body`, or `error` |
| `nova.html.select(html, cssSelector, JSON.stringify(fields))` | JSON string of row objects |
| `nova.storage.get(key)` | Stored string, or an empty string |
| `nova.storage.set(key, value)` | Empty string on success, error string on failure |
| `nova.log(message)` | Bounded diagnostic log |
| `nova.metadata.checkpoint(JSON.stringify(details))` | Empty string on success, error string otherwise; native fallback for details operations |
| `nova.metadata.available()` | Whether addon metadata is available outside a nested lookup |
| `nova.metadata.enrich(JSON.stringify(request))` | JSON string with normalized details, connections, and episode metadata, or `error` |
| `nova.metadata.resolveEpisode(JSON.stringify(request))` | JSON string with confirmed native episode identity or missing/ambiguous status |
| `nova.crypto.base64UrlEncode(value)` / `base64UrlDecode(value)` | UTF-8 opaque ID encoding |
| `nova.crypto.rc4Base64Url(value, key)` | Padded URL-safe RC4 output for the site's VRF convention |
| `nova.crypto.aes256CbcDecrypt(ciphertextBase64, keyBase64, ivBase64)` | UTF-8 plaintext with PKCS#7 padding removed, or an empty string on invalid input |
| `nova.crypto.hmacSha256Base64Url(value, key)` | Unpadded URL-safe HMAC-SHA256 signature |

CSS fields have `{ selector, value }`. An empty selector means the row itself;
`value` is `text`, `texts` (joined descendant text), `html`, or an attribute name.

## Runtime limits

Each operation gets a fresh QuickJS heap (32 MiB, 512 KiB stack), a 5-second
script budget excluding blocking HTTP time, and a whole-operation deadline
(AniKoto: 30 seconds). HTTP is HTTPS GET on port 443 to declared domains only.
Every redirect and DNS result is checked; private/reserved addresses, URL
credentials, and system proxy routing are denied. Response bodies and final
JSON output are capped at 2 MiB. AniKoto permits 32 requests, including
redirects, and 5 redirects per request. HTML inputs, selection rows, headers,
storage entries/writes, and logs have additional bounds. Storage is namespaced
and limited to the running session. The JS context has no filesystem, process,
socket API, native module loader, or unrestricted `fetch`.

## Playback and downloads

The provider resolves direct MP4/HLS links, MegaPlay's encrypted source API
(AES-256-CBC plus short-lived HMAC tokens), and Mewcdn playlist fragments and
host mappings. MegaPlay's `s=tcdn` image-wrapped segment variant is omitted:
standard mpv cannot demux its image prefix. Vidstream's normal HLS servers are
supported. Other embedded players remain unsupported.
`Referer`/`Origin` and other permitted request headers and external subtitle
files reach in-app mpv and survive resume prompts and Android decoder reloads.
English subtitles are ordered first when supplied by MegaPlay. Headers are
also persisted with MP4 download jobs.
Finite HLS episodes can also be downloaded through `nova-download`: the highest-bandwidth variant and its audio/subtitle groups, initialization maps, byte ranges and ordinary AES-128 keys are saved as a local playlist bundle. Provider headers accompany playlist, segment and key requests. Completed segments survive pause/resume; partial segments restart. Removing the episode download removes its entire bundle, including keys, subtitles and partial files. Automatic watched cleanup waits until the active HLS episode closes so the ending remains playable. Live playlists, DASH and unsupported encryption remain unavailable for offline download.

## Checks

```sh
cargo test -p nova-providers -p nova-download -p addons
cargo check --workspace --locked
```

Provider regressions use HTML/JSON fixtures, including VRF padding, real page
sizes, stable episode IDs across token refreshes, encrypted embeds and signed
tokens, subtitles, failed-server isolation, foreign library IDs, separate
seasons, split/merged episode sequences, reverse canonical stream aliases,
later seasons crowded out by side stories, configured AIOStreams/Torrentio
request routing, stale replies after alias refresh,
continuous episode numbering, metadata ambiguity, and sandbox
limits. Download tests use local HTTP servers. These checks do not
establish availability of the live site or individual video hosts.

## Tracker identity evidence

`ExternalIds` retains typed MAL/AniList anime and manga, IMDb, and TMDB movie/TV
claims alongside legacy fields. Stremio normalization accepts explicit prefixes,
official catalog URLs and known field aliases, retains conflicting claims, and
never interprets an undeclared bare number as an anime tracker ID. Addons keep
their original source and episode IDs. Tracking uses anime namespaces only;
confirmed canonical episode labels/aliases are context, not writable coverage.

The app retains typed identity evidence from preview/detail metadata for tracking.
It resolves the selected tracker directly or via AniList's official MAL
cross-reference, then offers bounded alias search and explicit manual alignment.
IMDb/TMDB sources remain supported by this manual path; no curated mapping dataset
is bundled. See [anime tracking behavior](../../docs/tracking-behavior.md).

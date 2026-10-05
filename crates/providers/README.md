# Content providers

Nova bundles AniKoto as a JavaScript provider. It appears in Settings → Addons
and in the series catalog/search flows. The `nova-provider://anikoto` transport
adapts its normalized output to the app's existing Stremio response parser, so
library entries, episode progress, metadata caches, downloads, and sync use the
same paths as other sources. The addon can be disabled or removed; it is
registered once per installation (`providers:bundled:v1`). Paste the transport
URL into Addons to reinstall it.

The Yuzono Android extensions contain Kotlin code for Aniyomi's APIs. Adding a
source here means porting its requests and parsing to Nova's JavaScript API.
This version loads trusted source files bundled at compile time.

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
HLS playback works through mpv; manifest downloads remain unsupported.

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
is bundled. See [anime tracking behavior](../../docs/tracking.md).

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

- `src/models.rs`: `ContentProvider` and normalized media, episode, stream,
  subtitle, descriptor, and request types. Pagination uses item offsets; media
  and stream requests carry their media type explicitly.
- `src/stremio.rs`: Stremio adapter to the normalized contract.
- `src/runtime.rs`: synchronous QuickJS invocation and injected capabilities.
- `src/host.rs`: HTTP policy, DNS validation/pinning, and bounded session storage.
- `src/anikoto.rs`: bundled source registration, protocol adaptation, and
  Cinemeta metadata enrichment through confirmed episode mappings.
- `src/matching.rs`: external ID/title/alias/year matching. Ambiguity leaves
  the source's own metadata and identity intact.
- `plugins/anikoto/`: source, permission manifest, addon-facing manifest, notice.

Invoke providers on background workers. The bundled source exposes
`globalThis.novaProvider.handle(request)` and returns a JSON-compatible value.
AniKoto accepts:

```js
{ op: "catalog", catalogId: "anikoto.popular", extra: { skip: "0" } }
{ op: "catalog", catalogId: "anikoto.search", extra: { search: "title", skip: "0" } }
{ op: "details", sourceId: "opaque-source-id" }
{ op: "streams", episodeId: "ep:opaque-episode-id" }
{ op: "lookupStreams", lookup: { mediaId: "tt5095466", mediaType: "series", title: "The Asterisk War", year: "2015", season: 1, episode: 13, absoluteEpisode: 13, seasonEpisodeCount: 24, seriesEpisodeCount: 24, episodeTitle: "Divine Revelations", released: "2016-04-02" } }
```

Catalogs return `MediaItem[]`; details return `{ media: MediaItem, episodes:
Episode[] }`; streams return `ProviderStream[]`. Model fields use snake case.
Persisted IDs are `provider_id:source_id`. AniKoto episodes encode the show path
and episode number, then look up expiring server tokens at playback time.
Cinemeta matches provide external IDs and fill missing metadata; they never
replace the source IDs.
The optional lookup has a separate 3-second deadline and a one-hour session
cache; its failure does not fail the source request.

Library shows from other addons also request AniKoto streams. The app builds
a contextual `resolve/series/...` request with the existing title/year and the
selected video's season, episode number, title, and release date, plus episode
counts when metadata has contiguous numbering. The plugin matches up to six
source candidates. A unique meaningful episode title can locate an episode
at a different number. For exact series families, it orders numbered seasons,
parts, and cours and aligns their episode counts against metadata boundaries
or a matching series total. This covers merged/split seasons, unequal cour
lengths, and continuous numbering without assuming a fixed cour size.
The native site's declared episode count supports ongoing parts; the selected
episode must still have servers. Missing parts, numbering gaps, duplicate
editions, conflicting years, specials, and ambiguous mappings are rejected.
Count alignment tolerates translated/edited episode titles; weaker numbering
fallbacks reject conflicting meaningful titles.
Shortened series titles require episode-title confirmation. Positive source
mappings are cached for one hour in bounded session storage; stream tokens
are always refreshed. Library IDs, episode history, downloads, and sync retain
the original addon identifiers.

The reverse path uses the same episode mapper. During AniKoto metadata loading,
the host finds a unique Cinemeta series and fetches its episode metadata with a
separate five-second deadline. The JavaScript `canonicalCandidate` and
`mapEpisodes` operations confirm canonical episode IDs without resolving streams.
The source entry you opened is always included, even when it is absent from
the current search pages. Its already-fetched episode rows are reused. Exact
series families and the selected season take priority over side stories under
the six-candidate limit, so later seasons are not displaced by OVAs/specials.
Confirmed IMDb episode aliases are attached to native videos as `novaStreamIds`
and retained by the existing episode cache. Matched canonical episodes also
supply meaningful titles, aired dates, thumbnails, and descriptions. Displayed
season/episode numbers follow the confirmed mapping (Slime S4 starts at S4E1;
the second Asterisk cour is E13–E24 in Cinemeta's merged first season). Only
episodes available in that native entry are shown, and their stable IDs still
encode native playback numbers. Canonical series metadata fills missing art,
descriptions, genres, and external IDs without replacing the source title/year.
For shortened canonical titles, two globally unique episode names at matching
positions and equal season counts can confirm the other positions; conflicting
anchors, repeated names, and missing counts do not prove that alignment.
Nullable optional metadata arrays are accepted by the Stremio parser.
Other installed stream addons use
the first alias their manifest accepts, including AIOStreams and Torrentio;
AniKoto requests continue using native IDs. Unresolved private AniKoto IDs are
not sent to foreign addons. Unavailable/ambiguous canonical metadata leaves
only native routing.
Mappings and matched metadata have a one-hour session cache (empty results:
30 seconds), invalidated when native episode availability changes. Host cache
entries are capped at 256 KiB under the shared 2 MiB session limit; larger
results remain usable but are not cached. JS storage keeps its 16 KiB entry cap.
Fresh aliases update a cached picker and restart an open stream search if its
selected episode gained or lost aliases; playback already in progress is left
alone. Addons requiring other ID namespaces need mappings for those namespaces.
Each stream search has a generation: old replies cannot change rows, loading
state, or addon pills after aliases cause the same episode to be queried again.
An open stream view also receives the refreshed selected episode's caption
and thumbnail without changing its selected episode or playback identity.

## Injected JavaScript API

| API | Result |
| --- | --- |
| `nova.http.get(url, JSON.stringify(headers))` | JSON string with `status`, `url`, `contentType`, `body`, or `error` |
| `nova.html.select(html, cssSelector, JSON.stringify(fields))` | JSON string of row objects |
| `nova.storage.get(key)` | Stored string, or an empty string |
| `nova.storage.set(key, value)` | Empty string on success, error string on failure |
| `nova.log(message)` | Bounded diagnostic log |
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

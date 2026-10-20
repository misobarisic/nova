# Catalog grouping protocol fixtures

`aiometadata-imdb-anime.json` is a small **synthetic, source-shaped** response,
not a recorded live response and not a claim about a particular title's mapping.
Its native IDs and artwork are deliberately fictional. It covers the supported
AIOMetadata IMDb episode compatibility output: canonical `tt…:season:episode`
IDs and explicit `imdb_id`, `imdbSeason`, `imdbEpisode` episode mapping fields,
while the native season/episode fields may still be relative to the anime entry.
The `anime` media type also verifies that custom catalog types stay native.

Primary sources inspected on 2026-10-20:

- [AIOMetadata `getMeta.js`](https://github.com/cedya77/aiometadata/blob/dev/addon/lib/getMeta.js),
  `buildAnimeResponse` and `buildKitsuAnimeResponse`: the IMDb compatibility path
  calls `enrichMalEpisodes`; the ordinary MAL/Kitsu path may have only native IDs.
- [AIOMetadata `id-mapper.js`](https://github.com/cedya77/aiometadata/blob/dev/addon/lib/id-mapper.js),
  `enrichMalEpisodes` and `enrichMalEpisodesFromTvdbBridge`: mapped canonical
  episode IDs and the explicit canonical episode triple.

Nova reads those fields through a provider-neutral adapter. An ordinary
`imdb_id`/`_imdbId` on a preview is not proof of a whole-series relationship.
Unknown, partially mapped, conflicting, special, or failed metadata retains its
original cards; the app must explain that grouping requires confirmation.

A compatible addon can alternatively declare an explicit relationship:

```json
{"novaCatalogGrouping":{"version":1,"kind":"series_member","parentIds":["imdb:tt1234567","tmdb:tv:123","tvdb:123"],"season":2}}
```

`season` is optional. Its omission permits Series grouping but never guesses a
season. All `parentIds` describe the same TV series; contradictory aliases
invalidate the whole connected component. The extension is a Nova contract,
not an existing AIOMetadata field. Specials and movies must not advertise this
relation. An absent season, malformed relation or unknown version is harmless.
The adapter supports canonical IMDb, typed TMDB-TV and TVDB series identities,
without changing tracker IDs. Display groups always retain original previews
and exact installation ownership. A multi-member group requires member choice.

//! Rust adaptation of Nuvio Mobile's TMDB More Like This flow.
//! Upstream: NuvioMedia/NuvioMobile, GPL-3.0, commit
//! 324afcffb962f1f7df45c5fa951131acec5a15a7, TmdbService.kt and
//! TmdbMetadataService.kt (ensureTmdbId / fetchMoreLikeThis).
//! Changes: typed identity checks, bounded transport, addon-compatible IDs.
use super::*;
use nova_providers::{ExternalId, IdNamespace, IdResolution};
use serde_json::Value;

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct Settings {
    key: String,
    language: String,
}

impl Settings {
    pub(super) fn from_settings(settings: &CacheSettings) -> Self {
        Self {
            key: settings.tmdb_api_key.trim().to_owned(),
            language: settings.language.code().to_owned(),
        }
    }

    pub(super) fn configured(&self) -> bool {
        self.key.len() == 32 && self.key.bytes().all(|b| b.is_ascii_hexdigit())
    }

    pub(super) fn authority(&self) -> (u64, &str) {
        (fnv1a(self.key.as_bytes()), &self.language)
    }

    fn url(&self, endpoint: &str, query: &[(&str, &str)]) -> String {
        let mut url = url::Url::parse(&format!("https://api.themoviedb.org/3/{endpoint}"))
            .expect("fixed TMDB origin and validated identity");
        url.query_pairs_mut()
            .append_pair("api_key", &self.key)
            .append_pair("language", &self.language)
            .extend_pairs(query.iter().copied());
        url.into()
    }
}

fn json(fetch: &mut impl FnMut(&str) -> Option<Vec<u8>>, url: &str) -> Option<Value> {
    serde_json::from_slice(&fetch(url)?).ok()
}

fn positive(value: &Value) -> Option<u32> {
    u32::try_from(value.as_u64()?).ok().filter(|id| *id > 0)
}

fn resolve_id(
    seed: &MetaPreview,
    media_type: &str,
    settings: &Settings,
    fetch: &mut impl FnMut(&str) -> Option<Vec<u8>>,
) -> Option<u32> {
    let mut source_id = seed.id.clone();
    // Stremio's legacy tmdb:N form carries its namespace in the media type.
    if let Some(legacy) = seed.id.strip_prefix("tmdb:")
        && legacy.bytes().all(|b| b.is_ascii_digit())
        && let Ok(id) = legacy.parse::<u32>()
        && id > 0
    {
        source_id = format!("tmdb:{media_type}:{id}");
    }
    let ids = nova_providers::normalize_external_ids(&seed.extra, &source_id, &seed.type_);
    let namespace = if media_type == "movie" {
        IdNamespace::TmdbMovie
    } else {
        IdNamespace::TmdbTv
    };
    match ids.resolve_id(namespace) {
        IdResolution::Unique(ExternalId::TmdbMovie(id) | ExternalId::TmdbTv(id)) => Some(id.get()),
        IdResolution::Conflict(_) => None,
        _ => {
            let IdResolution::Unique(ExternalId::Imdb(imdb)) = ids.resolve_id(IdNamespace::Imdb)
            else {
                return None;
            };
            let body = json(
                fetch,
                &settings.url(&format!("find/{imdb}"), &[("external_source", "imdb_id")]),
            )?;
            let results = body
                .get(if media_type == "movie" {
                    "movie_results"
                } else {
                    "tv_results"
                })?
                .as_array()?;
            // An ambiguous external-ID lookup must not pick an arbitrary show.
            let mut ids = results
                .iter()
                .filter_map(|row| positive(&row["id"]))
                .collect::<Vec<_>>();
            ids.sort_unstable();
            ids.dedup();
            (ids.len() == 1).then(|| ids[0])
        }
    }
}

fn text(row: &Value, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| {
        row.get(field)?
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    })
}

fn image(row: &Value, field: &str, size: &str) -> Option<String> {
    let path = row.get(field)?.as_str()?;
    (path.starts_with('/') && !path.starts_with("//") && !path.contains(['?', '#', '\\']))
        .then(|| format!("https://image.tmdb.org/t/p/{size}{path}"))
}

pub(super) fn recommendations(
    seed: &MetaPreview,
    installed: &[Installed],
    settings: &Settings,
    fetch: &mut impl FnMut(&str) -> Option<Vec<u8>>,
) -> Vec<MetaPreview> {
    if !settings.configured() {
        return Vec::new();
    }
    let media_type = match seed.type_.as_str() {
        "movie" => "movie",
        "series" => "tv",
        _ => return Vec::new(),
    };
    let Some(id) = resolve_id(seed, media_type, settings, fetch) else {
        return Vec::new();
    };
    let Some(body) = json(
        fetch,
        &settings.url(&format!("{media_type}/{id}/recommendations"), &[]),
    ) else {
        return Vec::new();
    };
    let Some(results) = body.get("results").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for row in results.iter().take(20) {
        if rows.len() == 12 {
            break;
        }
        let Some(tmdb) = positive(&row["id"]).filter(|candidate| *candidate != id) else {
            continue;
        };
        if !seen.insert(tmdb)
            || row["adult"].as_bool() == Some(true)
            || row
                .get("media_type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind != media_type)
        {
            continue;
        }
        let Some(name) = text(row, &["title", "name", "original_title", "original_name"]) else {
            continue;
        };
        let tmdb_id = format!("tmdb:{tmdb}");
        let accepts = |candidate: &str| {
            installed.iter().any(|a| {
                a.enabled && a.available && a.manifest.accepts("meta", &seed.type_, candidate)
            })
        };
        // Cinemeta and many stream addons accept IMDb only. Resolve before
        // publishing so choosing a recommendation follows normal detail/playback.
        let candidate_id = if !accepts("tt1") && accepts(&tmdb_id) {
            tmdb_id
        } else {
            let Some(external) = json(
                fetch,
                &settings.url(&format!("{media_type}/{tmdb}/external_ids"), &[]),
            ) else {
                continue;
            };
            let Some(imdb) = external
                .get("imdb_id")
                .and_then(Value::as_str)
                .filter(|id| matches!(ExternalId::parse(id), Some(ExternalId::Imdb(_))))
                .filter(|id| accepts(id))
            else {
                continue;
            };
            imdb.to_owned()
        };
        let mut extra = HashMap::new();
        extra.insert("tmdb_id".into(), tmdb.into());
        extra.insert("novaRecommendationSource".into(), "tmdb".into());
        let date = text(row, &["release_date", "first_air_date"]);
        rows.push(MetaPreview {
            id: candidate_id,
            type_: seed.type_.clone(),
            name,
            poster: image(row, "poster_path", "w500")
                .or_else(|| image(row, "backdrop_path", "w780")),
            background: image(row, "backdrop_path", "w1280"),
            description: text(row, &["overview"]),
            release_info: date.map(|date| Value::String(date.chars().take(4).collect())),
            imdb_rating: None, // TMDB votes are not IMDb ratings.
            extra,
            ..Default::default()
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings() -> Settings {
        Settings {
            key: "0123456789abcdef0123456789abcdef".into(),
            language: "hr".into(),
        }
    }

    fn addon(prefix: &str) -> Installed {
        Installed {
            url: "https://installed.example".into(), label: "Test".into(), enabled: true,
            available: true, generation: 1, configure_ok: None,
            manifest: serde_json::from_value(json!({"id":"test","name":"Test","version":"1.0.0","types":["series","movie"],"resources":[{"name":"meta","types":["series","movie"],"idPrefixes":[prefix]}],"catalogs":[]})).unwrap(),
        }
    }

    #[test]
    fn recommendations_resolve_typed_identity_preserve_order_and_open_with_imdb() {
        let seed = MetaPreview {
            id: "tt100".into(),
            type_: "series".into(),
            ..Default::default()
        };
        let mut requests = Vec::new();
        let rows = recommendations(&seed, &[addon("tt")], &settings(), &mut |url| {
            let url = url::Url::parse(url).unwrap();
            assert_eq!(url.host_str(), Some("api.themoviedb.org"));
            assert!(url.query_pairs().any(|(k, v)| k == "language" && v == "hr"));
            requests.push(url.path().to_owned());
            Some(serde_json::to_vec(&match url.path() {
                "/3/find/tt100" => json!({"tv_results":[{"id":1}],"movie_results":[{"id":999}]}),
                "/3/tv/1/recommendations" => json!({"results":[
                    {"id":2,"name":" First ","poster_path":"/a.jpg","backdrop_path":"/b.jpg","first_air_date":"2020-01-01","vote_average":8.9},
                    {"id":2,"name":"Duplicate"}, {"id":1,"name":"Seed"},
                    {"id":3,"original_name":"Second"}, {"id":4,"name":"Wrong medium","media_type":"movie"},
                    {"id":5,"name":"Adult","adult":true}, {"id":0,"name":"Invalid"}, {"id":6}]}),
                "/3/tv/2/external_ids" => json!({"imdb_id":"tt200"}),
                "/3/tv/3/external_ids" => json!({"imdb_id":"tt300"}),
                path => panic!("unexpected endpoint {path}"),
            }).unwrap())
        });
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["tt200", "tt300"]
        );
        assert_eq!(rows[0].name, "First");
        assert_eq!(rows[0].year_str().as_deref(), Some("2020"));
        assert_eq!(
            rows[0].poster.as_deref(),
            Some("https://image.tmdb.org/t/p/w500/a.jpg")
        );
        assert_eq!(rows[0].extra["tmdb_id"], 2);
        assert!(rows[0].imdb_rating.is_none());
        assert_eq!(requests.len(), 4);
    }

    #[test]
    fn missing_key_and_ambiguous_or_conflicting_identity_do_not_guess() {
        let mut seed = MetaPreview {
            id: "tt100".into(),
            type_: "movie".into(),
            ..Default::default()
        };
        assert!(
            recommendations(&seed, &[], &Settings::default(), &mut |_| panic!(
                "no key, no request"
            ))
            .is_empty()
        );
        assert!(
            recommendations(&seed, &[], &settings(), &mut |_| Some(
                br#"{"movie_results":[{"id":1},{"id":2}]}"#.to_vec()
            ))
            .is_empty()
        );
        seed.extra.insert("tmdb_id".into(), 1.into());
        seed.extra.insert("_tmdbId".into(), 2.into());
        assert!(
            recommendations(&seed, &[], &settings(), &mut |_| panic!(
                "conflicting IDs must not fetch"
            ))
            .is_empty()
        );
        let mut bad_key = settings();
        bad_key.key = "invalid".into();
        assert!(!bad_key.configured());
        assert_ne!(settings().authority(), bad_key.authority());
        bad_key = settings();
        bad_key.language = "en".into();
        assert_ne!(settings().authority(), bad_key.authority());
    }

    #[test]
    fn typed_tmdb_movies_use_movie_endpoint_and_filter_unsupported_results() {
        let seed = MetaPreview {
            id: "tmdb:movie:10".into(),
            type_: "movie".into(),
            ..Default::default()
        };
        let rows = recommendations(&seed, &[addon("tt")], &settings(), &mut |url| {
            Some(serde_json::to_vec(&if url.contains("/movie/10/recommendations?") {
                json!({"results":[{"id":20,"title":"Supported"},{"id":30,"title":"No IMDb ID"}]})
            } else if url.contains("/movie/20/external_ids?") { json!({"imdb_id":"tt200"}) }
            else if url.contains("/movie/30/external_ids?") { json!({"imdb_id":null}) }
            else { panic!("wrong media type or unnecessary ID lookup") }).unwrap())
        });
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "tt200");
        assert_eq!(rows[0].type_, "movie");
    }
}

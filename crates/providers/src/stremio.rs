use std::{collections::BTreeMap, sync::Arc};

use addons::{Addon, Manifest, MetaItem, MetaPreview, Stream};
use serde_json::Value;

use crate::{
    CatalogRequest, ContentProvider, Episode, ExternalIds, MediaItem, MediaRequest,
    ProviderCatalog, ProviderDescriptor, ProviderError, ProviderFuture, ProviderHost,
    ProviderStream, SearchRequest, StreamRequest,
};

/// The Stremio protocol adapter. Nova's app can keep its current wire and
/// response parser while the rest of the provider system uses normalized
/// models and provider-scoped opaque IDs.
pub struct StremioProvider {
    addon: Addon,
    manifest: Manifest,
    descriptor: ProviderDescriptor,
}

impl StremioProvider {
    pub fn new(addon: Addon, manifest: Manifest) -> Self {
        let catalogs = manifest
            .catalogs
            .iter()
            .map(|catalog| ProviderCatalog {
                id: catalog.id.clone(),
                name: catalog.name.clone(),
                media_type: catalog.type_.clone(),
                supports_search: catalog.supports_extra("search"),
                supports_pagination: catalog.supports_extra("skip"),
            })
            .collect();
        let descriptor = ProviderDescriptor {
            id: format!("stremio:{}", manifest.id),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            description: manifest.description.clone().unwrap_or_default(),
            media_types: manifest.types.clone(),
            catalogs,
        };
        Self {
            addon,
            manifest,
            descriptor,
        }
    }

    pub(crate) fn convert_preview(&self, preview: MetaPreview) -> MediaItem {
        let external_ids = normalize_external_ids(&preview.extra, &preview.id, &preview.type_);
        let title = preview.title();
        let year = preview.year_str();
        MediaItem {
            provider_id: self.descriptor.id.clone(),
            source_id: preview.id,
            media_type: preview.type_,
            title,
            aliases: string_values(&preview.extra, &["aliases", "aka", "akaNames"]),
            year,
            poster: preview.poster,
            background: preview.background,
            description: preview.description,
            genres: preview.genres,
            external_ids,
        }
    }

    pub(crate) fn convert_detail(&self, detail: MetaItem) -> (MediaItem, Vec<Episode>) {
        let mut item = self.convert_preview(detail.preview.clone());
        item.aliases = string_values(&detail.extra, &["aliases", "aka", "akaNames"]);
        // Detail and preview fields can carry different claims. Preserve both,
        // including conflicts, instead of replacing the preview's evidence.
        let mut fields = detail.preview.extra.clone();
        for (key, value) in &detail.extra {
            fields.entry(key.clone()).or_insert_with(|| value.clone());
        }
        item.external_ids =
            normalize_external_ids(&fields, &detail.preview.id, &detail.preview.type_);
        let detail_ids =
            normalize_external_ids(&detail.extra, &detail.preview.id, &detail.preview.type_);
        for id in detail_ids.typed {
            if !item.external_ids.typed.contains(&id) {
                item.external_ids.typed.push(id);
            }
        }

        for namespace in [
            crate::IdNamespace::Imdb,
            crate::IdNamespace::MalAnime,
            crate::IdNamespace::AnilistAnime,
        ] {
            if matches!(
                item.external_ids.resolve_id(namespace),
                crate::IdResolution::Conflict(_)
            ) {
                match namespace {
                    crate::IdNamespace::Imdb => item.external_ids.imdb = None,
                    crate::IdNamespace::MalAnime => item.external_ids.mal = None,
                    _ => item.external_ids.anilist = None,
                }
            }
        }
        let episodes = detail
            .videos
            .into_iter()
            .filter_map(|video| {
                let number = video.episode_number()?;
                let title = video.label();
                Some(Episode {
                    provider_id: self.descriptor.id.clone(),
                    source_id: video.id,
                    parent_id: detail.preview.id.clone(),
                    number,
                    season: video.season.unwrap_or(1),
                    title,
                    released: video.released,
                    thumbnail: video.thumbnail,
                })
            })
            .collect();
        (item, episodes)
    }
}

impl ContentProvider for StremioProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn browse<'a>(
        &'a self,
        host: Arc<dyn ProviderHost>,
        request: CatalogRequest,
    ) -> ProviderFuture<'a, Vec<MediaItem>> {
        Box::pin(async move {
            let catalog = self
                .manifest
                .catalog_for(&request.media_type, &request.catalog_id)
                .ok_or_else(|| ProviderError::Unsupported("unknown Stremio catalog".into()))?;
            let skip = request.skip.to_string();
            let mut extras = Vec::new();
            if catalog.supports_extra("skip") && request.skip > 0 {
                extras.push(("skip", skip.as_str()));
            }
            if catalog.supports_extra("genre")
                && let Some(genre) = request.genre.as_deref()
            {
                extras.push(("genre", genre));
            }
            let response = host
                .get(
                    &self
                        .addon
                        .catalog_url(&request.media_type, &request.catalog_id, &extras),
                    &BTreeMap::new(),
                )
                .map_err(host_error)?;
            let previews = Addon::parse_catalog(&response.body)
                .map_err(|error| ProviderError::InvalidData(error.to_string()))?;
            Ok(previews
                .into_iter()
                .map(|preview| self.convert_preview(preview))
                .collect())
        })
    }

    fn search<'a>(
        &'a self,
        host: Arc<dyn ProviderHost>,
        request: SearchRequest,
    ) -> ProviderFuture<'a, Vec<MediaItem>> {
        Box::pin(async move {
            let catalog = self
                .manifest
                .catalogs_for_type(&request.media_type)
                .find(|catalog| catalog.supports_extra("search"))
                .ok_or_else(|| {
                    ProviderError::Unsupported("Stremio addon has no search catalog".into())
                })?;
            let search = request.query;
            let skip = request.skip.to_string();
            let mut extras = vec![("search", search.as_str())];
            if request.skip > 0 && catalog.supports_extra("skip") {
                extras.push(("skip", skip.as_str()));
            }
            let response = host
                .get(
                    &self
                        .addon
                        .catalog_url(&request.media_type, &catalog.id, &extras),
                    &BTreeMap::new(),
                )
                .map_err(host_error)?;
            let previews = Addon::parse_catalog(&response.body)
                .map_err(|error| ProviderError::InvalidData(error.to_string()))?;
            Ok(previews
                .into_iter()
                .map(|preview| self.convert_preview(preview))
                .collect())
        })
    }

    fn details<'a>(
        &'a self,
        host: Arc<dyn ProviderHost>,
        request: MediaRequest,
    ) -> ProviderFuture<'a, MediaItem> {
        Box::pin(async move {
            let media_type = &request.media_type;
            let response = host
                .get(
                    &self.addon.meta_url(media_type, &request.source_id),
                    &BTreeMap::new(),
                )
                .map_err(host_error)?;
            let detail = Addon::parse_meta(&response.body)
                .map_err(|error| ProviderError::InvalidData(error.to_string()))?
                .ok_or_else(|| {
                    ProviderError::InvalidData("Stremio addon returned no metadata".into())
                })?;
            Ok(self.convert_detail(detail).0)
        })
    }

    fn episodes<'a>(
        &'a self,
        host: Arc<dyn ProviderHost>,
        request: MediaRequest,
    ) -> ProviderFuture<'a, Vec<Episode>> {
        Box::pin(async move {
            let media_type = &request.media_type;
            let response = host
                .get(
                    &self.addon.meta_url(media_type, &request.source_id),
                    &BTreeMap::new(),
                )
                .map_err(host_error)?;
            let detail = Addon::parse_meta(&response.body)
                .map_err(|error| ProviderError::InvalidData(error.to_string()))?
                .ok_or_else(|| {
                    ProviderError::InvalidData("Stremio addon returned no metadata".into())
                })?;
            Ok(self.convert_detail(detail).1)
        })
    }

    fn streams<'a>(
        &'a self,
        host: Arc<dyn ProviderHost>,
        request: StreamRequest,
    ) -> ProviderFuture<'a, Vec<ProviderStream>> {
        Box::pin(async move {
            let media_type = &request.media_type;
            let response = host
                .get(
                    &self.addon.stream_url(media_type, &request.episode_id),
                    &BTreeMap::new(),
                )
                .map_err(host_error)?;
            let streams = Addon::parse_streams(&response.body)
                .map_err(|error| ProviderError::InvalidData(error.to_string()))?;
            Ok(streams
                .into_iter()
                .enumerate()
                .filter_map(|(index, stream)| convert_stream(index, stream))
                .collect())
        })
    }
}

fn host_error(error: crate::ProviderHostError) -> ProviderError {
    ProviderError::Host(error.to_string())
}

fn convert_stream(index: usize, stream: Stream) -> Option<ProviderStream> {
    let url = stream.web_url()?;
    let request_headers = stream
        .extra
        .get("behaviorHints")
        .and_then(|value| value.get("proxyHeaders"))
        .and_then(|value| value.get("request"))
        .and_then(Value::as_object)
        .map(|headers| {
            headers
                .iter()
                .filter_map(|(name, value)| {
                    value.as_str().map(|value| (name.clone(), value.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(ProviderStream {
        id: format!("{}", index + 1),
        title: stream.label(),
        description: stream
            .description
            .or(stream.title_legacy)
            .unwrap_or_default(),
        url,
        headers: request_headers,
        subtitles: stream
            .subtitles
            .into_iter()
            .map(|subtitle| crate::models::ProviderSubtitle {
                url: subtitle.url,
                language: subtitle.lang,
            })
            .collect(),
    })
}

pub fn normalize_external_ids(
    fields: &std::collections::HashMap<String, Value>,
    source_id: &str,
    media_type: &str,
) -> ExternalIds {
    use crate::{ExternalId, IdNamespace, IdResolution};
    let mut ids = ExternalIds::default();
    if let Some(value) = fields.get("novaExternalIds")
        && let Ok(typed) = serde_json::from_value::<Vec<ExternalId>>(value.clone())
    {
        ids.typed.extend(typed);
    }
    let namespaces = [
        (IdNamespace::Imdb, &["imdb_id", "imdbId", "imdb"][..]),
        (
            IdNamespace::MalAnime,
            &["mal_id", "malId", "mal", "myanimelist_id"][..],
        ),
        (
            IdNamespace::AnilistAnime,
            &["anilist_id", "anilistId", "anilist", "aniListId"][..],
        ),
        (IdNamespace::TmdbTv, &["tmdb_tv_id", "tmdbTvId"][..]),
        (
            IdNamespace::TmdbMovie,
            &["tmdb_movie_id", "tmdbMovieId"][..],
        ),
    ];
    for (namespace, names) in namespaces {
        for name in names {
            if let Some(value) = fields.get(*name).and_then(value_string)
                && let Some(id) = ExternalId::from_field(namespace, &value)
                && !ids.typed.contains(&id)
            {
                ids.typed.push(id);
            }
        }
    }
    if let Some(id) = ExternalId::parse(source_id)
        && !ids.typed.contains(&id)
    {
        ids.typed.push(id);
    }
    // Stremio movie/series is explicit type evidence for legacy TMDB fields.
    // Other media types retain untyped values without guessing a catalog kind.
    let tmdb_kind = match media_type {
        "series" => Some(IdNamespace::TmdbTv),
        "movie" => Some(IdNamespace::TmdbMovie),
        _ => None,
    };
    for name in ["tmdb_id", "tmdbId", "tmdb"] {
        if let Some(value) = fields.get(name).and_then(value_string)
            && value.len() <= 512
        {
            if ids.tmdb.is_none() {
                ids.tmdb = Some(value.clone());
            }
            if let Some(id) = tmdb_kind.and_then(|kind| ExternalId::from_field(kind, &value))
                && !ids.typed.contains(&id)
            {
                ids.typed.push(id);
            }
        }
    }
    for namespace in [
        IdNamespace::Imdb,
        IdNamespace::MalAnime,
        IdNamespace::AnilistAnime,
    ] {
        if let IdResolution::Unique(id) = ids.resolve_id(namespace) {
            let field = match namespace {
                IdNamespace::Imdb => &mut ids.imdb,
                IdNamespace::MalAnime => &mut ids.mal,
                _ => &mut ids.anilist,
            };
            *field = Some(id.value());
        }
    }
    // Retain bounded opaque IDs from an explicitly declared extension map.
    if let Some(other) = fields.get("externalIds").and_then(Value::as_object) {
        for (key, value) in other.iter().take(32) {
            if key.len() <= 64
                && let Some(value) = value_string(value)
                && value.len() <= 512
            {
                ids.other.insert(key.clone(), value);
            }
        }
    }
    ids
}

fn string_values(fields: &std::collections::HashMap<String, Value>, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .find_map(|name| fields.get(*name))
        .and_then(|value| match value {
            Value::Array(values) => Some(values.iter().filter_map(value_string).collect()),
            Value::String(value) => Some(
                value
                    .split('|')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

fn value_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use crate::{ExternalId, IdNamespace, IdResolution};
    use serde_json::json;

    fn fields(value: Value) -> std::collections::HashMap<String, Value> {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn native_tracker_ids_do_not_require_imdb() {
        let ids = normalize_external_ids(
            &fields(json!({"mal_id":123,"anilistId":"https://anilist.co/anime/456/title"})),
            "source-slug",
            "series",
        );
        assert_eq!(ids.mal.as_deref(), Some("123"));
        assert_eq!(ids.anilist.as_deref(), Some("456"));
        assert_eq!(ids.imdb, None);
        assert_eq!(
            ids.resolve_id(IdNamespace::AnilistAnime),
            IdResolution::Unique(ExternalId::parse("anilist:456").unwrap())
        );
    }

    #[test]
    fn conflicting_aliases_suspend_selection() {
        let ids = normalize_external_ids(
            &fields(json!({"mal_id":123,"malId":456})),
            "mal:789",
            "series",
        );
        assert_eq!(ids.mal, None);
        assert!(
            matches!(ids.resolve_id(IdNamespace::MalAnime), IdResolution::Conflict(values) if values.len() == 3)
        );
        let ids = normalize_external_ids(
            &fields(json!({"mal_id":123,"malId":"00123"})),
            "123",
            "series",
        );
        assert_eq!(ids.mal.as_deref(), Some("123"));
    }

    #[test]
    fn tmdb_requires_kind_and_bad_values_are_not_accepted() {
        let ids = normalize_external_ids(
            &fields(json!({"tmdb_id":123,"mal_id":-1,"anilist_id":0})),
            "123",
            "anime",
        );
        assert!(ids.typed.is_empty());
        assert_eq!(ids.tmdb.as_deref(), Some("123"));
        let ids = normalize_external_ids(&fields(json!({"tmdb_id":123})), "123", "series");
        assert_eq!(
            ids.resolve_id(IdNamespace::TmdbTv),
            IdResolution::Unique(ExternalId::parse("tmdb:tv:123").unwrap())
        );
    }
    #[test]
    fn detail_keeps_source_identity_and_all_preview_claims() {
        let manifest =
            serde_json::from_value(json!({"id":"test","name":"Test","version":"1"})).unwrap();
        let provider = StremioProvider::new(Addon::new("https://example.com").unwrap(), manifest);
        let preview = MetaPreview {
            id: "tt123".into(),
            type_: "series".into(),
            extra: fields(json!({"mal_id":123})),
            ..Default::default()
        };
        let (item, _) = provider.convert_detail(MetaItem {
            preview,
            extra: fields(json!({"mal_id":456,"anilistId":789})),
            ..Default::default()
        });
        assert_eq!(item.source_id, "tt123");
        assert_eq!(item.external_ids.imdb.as_deref(), Some("tt123"));
        assert_eq!(item.external_ids.anilist.as_deref(), Some("789"));
        assert_eq!(item.external_ids.mal, None);
        assert!(matches!(
            item.external_ids.resolve_id(IdNamespace::MalAnime),
            IdResolution::Conflict(_)
        ));
    }
}

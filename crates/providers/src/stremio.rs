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
        let mut external_ids = external_ids(&preview.extra);
        if external_ids.imdb.is_none()
            && preview.id.len() > 2
            && preview.id.starts_with("tt")
            && preview.id[2..].bytes().all(|byte| byte.is_ascii_digit())
        {
            external_ids.imdb = Some(preview.id.clone());
        }
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

    fn convert_detail(&self, detail: MetaItem) -> (MediaItem, Vec<Episode>) {
        let mut item = self.convert_preview(detail.preview.clone());
        item.aliases = string_values(&detail.extra, &["aliases", "aka", "akaNames"]);
        item.external_ids = external_ids(&detail.extra);
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

fn external_ids(fields: &std::collections::HashMap<String, Value>) -> ExternalIds {
    let get = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| fields.get(*name).and_then(value_string))
    };
    ExternalIds {
        imdb: get(&["imdb_id", "imdbId", "imdb"]),
        tmdb: get(&["tmdb_id", "tmdbId", "tmdb"]),
        mal: get(&["mal_id", "malId", "mal"]),
        other: BTreeMap::new(),
    }
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

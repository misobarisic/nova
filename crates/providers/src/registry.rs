use crate::{
    CatalogRequest, ContentProvider, Episode, MediaItem, MediaRequest, PluginManifest,
    PluginRuntime, ProviderCatalog, ProviderDescriptor, ProviderError, ProviderFuture,
    ProviderHost, ProviderHostError, ProviderStream, ScopedHttpHost, SearchRequest,
    StreamLookupRequest, StreamRequest,
};
use addons::Manifest;
use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use url::Url;

pub const ANIKOTO_PROVIDER_URL: &str = "nova-provider://anikoto";
const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;

pub struct BundledProvider {
    pub url: &'static str,
    pub registration_key: &'static str,
    pub plugin_manifest: &'static str,
    pub addon_manifest: &'static str,
    pub source: &'static str,
}

pub fn bundled_providers() -> &'static [BundledProvider] {
    static PROVIDERS: &[BundledProvider] = &[BundledProvider {
        url: ANIKOTO_PROVIDER_URL,
        registration_key: "providers:bundled:v1",
        plugin_manifest: include_str!("../plugins/anikoto/manifest.json"),
        addon_manifest: include_str!("../plugins/anikoto/stremio-manifest.json"),
        source: include_str!("../plugins/anikoto/index.js"),
    }];
    PROVIDERS
}

pub fn provider_owns_id(provider_url: &str, id: &str) -> bool {
    Url::parse(provider_url)
        .ok()
        .filter(|u| u.scheme() == "nova-provider")
        .and_then(|u| u.host_str().map(str::to_owned))
        .is_some_and(|provider| id.starts_with(&format!("{provider}:")))
}

pub fn private_provider_id(id: &str) -> bool {
    bundled_providers()
        .iter()
        .any(|p| provider_owns_id(p.url, id))
}

pub fn supports_contextual_streams(provider_url: &str) -> bool {
    bundled_providers()
        .iter()
        .find(|p| p.url == provider_url.trim_end_matches('/'))
        .and_then(|p| serde_json::from_str::<PluginManifest>(p.plugin_manifest).ok())
        .is_some_and(|p| p.capabilities.contextual_streams)
}

pub fn stream_lookup_url(
    provider_url: &str,
    request: &StreamLookupRequest,
) -> Result<String, ProviderHostError> {
    validate_stream_lookup(request)?;
    if !supports_contextual_streams(provider_url) {
        return Err(ProviderHostError(
            "provider has no contextual streams".into(),
        ));
    }
    let data = serde_json::to_string(request).map_err(|e| ProviderHostError(e.to_string()))?;
    if data.len() > 8192 {
        return Err(ProviderHostError(
            "source lookup exceeded its byte limit".into(),
        ));
    }
    let mut url = Url::parse(provider_url).map_err(|e| ProviderHostError(e.to_string()))?;
    url.path_segments_mut()
        .map_err(|_| ProviderHostError("invalid provider URL".into()))?
        .extend(["resolve", &request.media_type, &format!("{data}.json")]);
    Ok(url.to_string())
}

fn validate_stream_lookup(request: &StreamLookupRequest) -> Result<(), ProviderHostError> {
    if request.media_type != "series"
        || request.media_id.is_empty()
        || request.title.trim().is_empty()
        || request.title.len() > 1024
        || request.media_id.len() > 1024
        || !(1..=100).contains(&request.season)
        || !(1..=10_000).contains(&request.episode)
        || request
            .season_episode_count
            .is_some_and(|n| !(1..=10_000).contains(&n))
        || request
            .series_episode_count
            .is_some_and(|n| !(1..=1_000_000).contains(&n))
    {
        return Err(ProviderHostError("invalid source stream lookup".into()));
    }
    Ok(())
}

pub fn fetch_builtin_addon(raw_url: &str) -> Option<Result<Vec<u8>, ProviderHostError>> {
    fetch_builtin_addon_with_timeout(raw_url, Duration::from_secs(60))
}

pub fn fetch_builtin_addon_with_timeout(
    raw_url: &str,
    timeout: Duration,
) -> Option<Result<Vec<u8>, ProviderHostError>> {
    let url = match Url::parse(raw_url) {
        Ok(url) if url.scheme() == "nova-provider" => url,
        _ => return None,
    };
    let Some(definition) = bundled_providers().iter().find(|p| {
        Url::parse(p.url)
            .ok()
            .is_some_and(|base| base.host_str() == url.host_str())
    }) else {
        return Some(Err(ProviderHostError("unknown bundled provider".into())));
    };
    let path = decoded_path_segments(&url);
    if path.last().is_some_and(|s| s == "manifest.json") {
        return Some(Ok(definition.addon_manifest.as_bytes().to_vec()));
    }
    let execute = || {
        let mut manifest: PluginManifest = serde_json::from_str(definition.plugin_manifest)
            .map_err(|e| ProviderHostError(e.to_string()))?;
        manifest.limits.request_timeout_ms = manifest
            .limits
            .request_timeout_ms
            .min(timeout.as_millis().clamp(1, 60_000) as u64);
        let addon: Manifest = serde_json::from_str(definition.addon_manifest)
            .map_err(|e| ProviderHostError(e.to_string()))?;
        let request = parse_protocol_request(&path, &manifest.id)?;
        let state = crate::host::session_state();
        let host = Arc::new(ScopedHttpHost::new(
            state,
            manifest.permissions.domains.clone(),
            Duration::from_millis(manifest.limits.request_timeout_ms.min(60_000)),
            manifest.limits.response_bytes.min(RESPONSE_LIMIT),
            manifest.limits.redirects,
            manifest.limits.requests_per_operation,
        ));
        let provider = JsProvider::new(manifest, addon, definition.source);
        let value = provider
            .protocol_response(host, request)
            .map_err(|e| ProviderHostError(e.to_string()))?;
        serde_json::to_vec(&value).map_err(|e| ProviderHostError(e.to_string()))
    };
    Some(execute())
}

struct JsProvider {
    descriptor: ProviderDescriptor,
    runtime: PluginRuntime,
}

impl JsProvider {
    fn new(manifest: PluginManifest, addon: Manifest, source: &str) -> Self {
        Self {
            descriptor: ProviderDescriptor {
                id: manifest.id.clone(),
                name: manifest.name.clone(),
                version: manifest.version.clone(),
                description: addon.description.unwrap_or_default(),
                media_types: addon.types,
                catalogs: addon
                    .catalogs
                    .iter()
                    .map(|c| ProviderCatalog {
                        id: c.id.clone(),
                        name: c.name.clone(),
                        media_type: c.type_.clone(),
                        supports_search: c.supports_extra("search"),
                        supports_pagination: c.supports_extra("skip"),
                    })
                    .collect(),
            },
            runtime: PluginRuntime::new(manifest, source),
        }
    }
    fn call(&self, host: Arc<dyn ProviderHost>, request: Value) -> Result<Value, ProviderError> {
        self.runtime.invoke(host, &request)
    }
    fn protocol_response(
        &self,
        host: Arc<dyn ProviderHost>,
        request: ProtocolRequest,
    ) -> Result<Value, ProviderError> {
        match request {
            ProtocolRequest::Catalog { catalog_id, extra } => {
                let result = self.call(
                    host,
                    json!({"op":"catalog", "catalogId":catalog_id, "extra":extra}),
                )?;
                let items = result
                    .as_array()
                    .ok_or_else(|| {
                        ProviderError::InvalidData("provider catalog result was not a list".into())
                    })?
                    .iter()
                    .map(|value| {
                        let item: MediaItem = serde_json::from_value(value.clone())
                            .map_err(|error| ProviderError::InvalidData(error.to_string()))?;
                        if item.provider_id != self.descriptor.id || item.source_id.is_empty() {
                            return Err(ProviderError::InvalidData(
                                "provider catalog identity mismatch".into(),
                            ));
                        }
                        Ok(media_preview(item))
                    })
                    .collect::<Result<Vec<_>, ProviderError>>()?;
                Ok(json!({"metas":items}))
            }
            ProtocolRequest::Meta { source_id } => {
                let result =
                    self.call(host.clone(), json!({"op":"details", "sourceId":source_id}))?;
                let item: MediaItem =
                    serde_json::from_value(result.get("media").cloned().unwrap_or(Value::Null))
                        .map_err(|error| ProviderError::InvalidData(error.to_string()))?;
                if item.provider_id != self.descriptor.id || item.source_id != source_id {
                    return Err(ProviderError::InvalidData(
                        "provider details identity mismatch".into(),
                    ));
                }
                let episodes = result
                    .get("episodes")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|value| {
                        let episode: Episode = serde_json::from_value(value)
                            .map_err(|error| ProviderError::InvalidData(error.to_string()))?;
                        if episode.provider_id != self.descriptor.id || episode.source_id.is_empty()
                        {
                            return Err(ProviderError::InvalidData(
                                "provider episode identity mismatch".into(),
                            ));
                        }
                        let video = json!({
                            "id": episode.stable_id(),
                            "name": episode.title,
                            "season": episode.season,
                            "episode": episode.number,
                            "number": episode.number,
                            "released": episode.released,
                            "thumbnail": episode.thumbnail,
                        });
                        Ok(video)
                    })
                    .collect::<Result<Vec<_>, ProviderError>>()?;
                let mut meta = media_preview(item);
                meta.as_object_mut()
                    .expect("preview is an object")
                    .insert("videos".into(), Value::Array(episodes));
                if let Some(enrichment) = result.get("enrichment") {
                    let enrichment: crate::EnrichmentResult =
                        serde_json::from_value(enrichment.clone())
                            .map_err(|error| ProviderError::InvalidData(error.to_string()))?;
                    crate::apply_enrichment(&mut meta, &enrichment);
                }
                Ok(json!({"meta":meta}))
            }
            request @ (ProtocolRequest::Streams { .. } | ProtocolRequest::Lookup { .. }) => {
                let request = match request {
                    ProtocolRequest::Streams { episode_id } => {
                        json!({"op":"streams", "episodeId":episode_id})
                    }
                    ProtocolRequest::Lookup { request } => {
                        json!({"op":"lookupStreams", "lookup":request})
                    }
                    _ => unreachable!(),
                };
                let result = self.call(host, request)?;
                let streams = result
                    .as_array()
                    .ok_or_else(|| ProviderError::InvalidData("provider stream result was not a list".into()))?
                    .iter()
                    .map(|value| {
                        let stream: ProviderStream = serde_json::from_value(value.clone())
                            .map_err(|error| ProviderError::InvalidData(error.to_string()))?;
                        let headers = stream
                            .headers
                            .iter()
                            .map(|(name, value)| (name.clone(), Value::String(value.clone())))
                            .collect::<serde_json::Map<_, _>>();
                        Ok(json!({
                            "name":stream.title,
                            "description":stream.description,
                            "url":stream.url,
                            "subtitles":stream.subtitles.iter().enumerate().map(|(index, subtitle)| json!({
                                "id":index.to_string(),
                                "url":subtitle.url,
                                "lang":subtitle.language,
                            })).collect::<Vec<_>>(),
                            "behaviorHints":{"proxyHeaders":{"request":headers}},
                        }))
                    })
                    .collect::<Result<Vec<_>, ProviderError>>()?;
                Ok(json!({"streams":streams}))
            }
        }
    }
}

impl ContentProvider for JsProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn browse<'a>(
        &'a self,
        host: Arc<dyn ProviderHost>,
        request: CatalogRequest,
    ) -> ProviderFuture<'a, Vec<MediaItem>> {
        Box::pin(async move {
            let value = self.call(
                host,
                json!({
                    "op":"catalog",
                    "catalogId":request.catalog_id,
                    "extra":{"skip":request.skip.to_string(),"genre":request.genre},
                }),
            )?;
            serde_json::from_value(value)
                .map_err(|error| ProviderError::InvalidData(error.to_string()))
        })
    }

    fn search<'a>(
        &'a self,
        host: Arc<dyn ProviderHost>,
        request: SearchRequest,
    ) -> ProviderFuture<'a, Vec<MediaItem>> {
        Box::pin(async move {
            let value = self.call(
                host,
                json!({"op":"catalog", "catalogId":self.descriptor.catalogs.iter().find(|c| c.media_type == request.media_type && c.supports_search).map(|c| c.id.as_str()).ok_or_else(|| ProviderError::Unsupported("provider has no search catalog".into()))?, "extra":{
                    "search":request.query,
                    "skip":request.skip.to_string(),
                }}),
            )?;
            serde_json::from_value(value)
                .map_err(|error| ProviderError::InvalidData(error.to_string()))
        })
    }

    fn details<'a>(
        &'a self,
        host: Arc<dyn ProviderHost>,
        request: MediaRequest,
    ) -> ProviderFuture<'a, MediaItem> {
        Box::pin(async move {
            let value = self.call(host, json!({"op":"details", "sourceId":request.source_id}))?;
            serde_json::from_value(value.get("media").cloned().unwrap_or(Value::Null))
                .map_err(|error| ProviderError::InvalidData(error.to_string()))
        })
    }

    fn episodes<'a>(
        &'a self,
        host: Arc<dyn ProviderHost>,
        request: MediaRequest,
    ) -> ProviderFuture<'a, Vec<Episode>> {
        Box::pin(async move {
            let value = self.call(host, json!({"op":"details", "sourceId":request.source_id}))?;
            serde_json::from_value(value.get("episodes").cloned().unwrap_or(Value::Null))
                .map_err(|error| ProviderError::InvalidData(error.to_string()))
        })
    }

    fn streams<'a>(
        &'a self,
        host: Arc<dyn ProviderHost>,
        request: StreamRequest,
    ) -> ProviderFuture<'a, Vec<ProviderStream>> {
        Box::pin(async move {
            let value = self.call(
                host,
                json!({"op":"streams", "episodeId":request.episode_id}),
            )?;
            serde_json::from_value(value)
                .map_err(|error| ProviderError::InvalidData(error.to_string()))
        })
    }
}

enum ProtocolRequest {
    Catalog {
        catalog_id: String,
        extra: HashMap<String, String>,
    },
    Meta {
        source_id: String,
    },
    Streams {
        episode_id: String,
    },
    Lookup {
        request: StreamLookupRequest,
    },
}

fn parse_protocol_request(
    segments: &[String],
    provider_id: &str,
) -> Result<ProtocolRequest, ProviderHostError> {
    match segments.first().map(String::as_str) {
        Some("catalog") if segments.len() >= 3 => {
            let mut catalog_id = segments[2].clone();
            let mut extra = HashMap::new();
            for segment in segments.iter().skip(3) {
                let segment = segment.strip_suffix(".json").unwrap_or(segment);
                if let Some((key, value)) = segment.split_once('=') {
                    extra.insert(key.to_owned(), value.to_owned());
                }
            }
            if catalog_id.ends_with(".json") {
                catalog_id.truncate(catalog_id.len() - 5);
            }
            Ok(ProtocolRequest::Catalog { catalog_id, extra })
        }
        Some("meta") if segments.len() >= 3 => {
            let source_id = segments[2].strip_suffix(".json").unwrap_or(&segments[2]);
            Ok(ProtocolRequest::Meta {
                source_id: source_id
                    .strip_prefix(&format!("{provider_id}:"))
                    .unwrap_or(source_id)
                    .to_owned(),
            })
        }
        Some("stream") if segments.len() >= 3 => {
            let episode_id = segments[2].strip_suffix(".json").unwrap_or(&segments[2]);
            Ok(ProtocolRequest::Streams {
                episode_id: episode_id
                    .strip_prefix(&format!("{provider_id}:"))
                    .unwrap_or(episode_id)
                    .to_owned(),
            })
        }
        Some("resolve") if segments.len() == 3 && segments[1] == "series" => {
            let data = segments[2].strip_suffix(".json").unwrap_or(&segments[2]);
            if data.len() > 8192 {
                return Err(ProviderHostError(
                    "source lookup exceeded its byte limit".into(),
                ));
            }
            let request: StreamLookupRequest =
                serde_json::from_str(data).map_err(|error| ProviderHostError(error.to_string()))?;
            validate_stream_lookup(&request)?;
            Ok(ProtocolRequest::Lookup { request })
        }
        _ => Err(ProviderHostError(
            "unknown bundled provider endpoint".into(),
        )),
    }
}

fn decoded_path_segments(url: &Url) -> Vec<String> {
    url.path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .map(|segment| percent_decode_str(segment).decode_utf8_lossy().into_owned())
        .collect()
}

pub(crate) fn media_preview(media: MediaItem) -> Value {
    let stable_id = media.stable_id();
    let typed = media.external_ids.typed.clone();
    let mut extra = serde_json::Map::new();
    if !typed.is_empty() {
        extra.insert("novaExternalIds".into(), json!(typed));
    }
    if let Some(year) = media.year {
        extra.insert("year".into(), Value::String(year.clone()));
        extra.insert("releaseInfo".into(), Value::String(year));
    }
    if !media.aliases.is_empty() {
        extra.insert("aliases".into(), json!(media.aliases));
    }
    if let Some(id) = media.external_ids.imdb {
        extra.insert("imdb_id".into(), Value::String(id));
    }
    if let Some(id) = media.external_ids.tmdb {
        extra.insert("tmdb_id".into(), Value::String(id));
    }
    if let Some(id) = media.external_ids.mal {
        extra.insert("mal_id".into(), Value::String(id));
    }
    if let Some(id) = media.external_ids.anilist {
        extra.insert("anilist_id".into(), Value::String(id));
    }
    for (key, value) in media.external_ids.other {
        extra.insert(key, Value::String(value));
    }
    let mut preview = json!({
        "id":stable_id,
        "type":media.media_type,
        "name":media.title,
        "genres":media.genres,
        "extra":extra,
    });
    let object = preview.as_object_mut().expect("preview is an object");
    for (field, value) in [
        ("poster", media.poster),
        ("background", media.background),
        ("logo", media.logo),
        ("description", media.description),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            object.insert(field.into(), Value::String(value));
        }
    }
    if let Some(extra) = object.remove("extra")
        && let Some(extra) = extra.as_object()
    {
        object.extend(
            extra
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    preview
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;

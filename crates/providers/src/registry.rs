use crate::models::ProviderResolveError;
use crate::{
    CatalogRequest, ContentProvider, Episode, MediaItem, MediaRequest, PluginManifest,
    PluginRuntime, ProviderCatalog, ProviderDescriptor, ProviderError, ProviderFuture,
    ProviderHost, ProviderHostError, ProviderStream, ScopedHttpHost, SearchRequest,
    StreamLookupRequest, StreamRequest,
};
use addons::Manifest;
use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use url::Url;

pub const ANIKOTO_PROVIDER_URL: &str = "nova-provider://anikoto";
pub const SUPERNOVA_PROVIDER_URL: &str = "nova-provider://supernova";
const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;

/// User-selected contributions; metadata includes catalog browsing and search.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ProviderCapabilities {
    pub streams: bool,
    pub metadata: bool,
}
impl Default for ProviderCapabilities {
    fn default() -> Self {
        Self {
            streams: true,
            metadata: true,
        }
    }
}
fn capability_configs() -> &'static Mutex<HashMap<String, ProviderCapabilities>> {
    static CONFIGS: OnceLock<Mutex<HashMap<String, ProviderCapabilities>>> = OnceLock::new();
    CONFIGS.get_or_init(Default::default)
}
pub fn supports_capability_config(url: &str) -> bool {
    bundled_providers().iter().any(|p| {
        p.url == url.trim_end_matches('/')
            && serde_json::from_str::<PluginManifest>(p.plugin_manifest)
                .is_ok_and(|m| m.capabilities.configurable_capabilities)
    })
}
pub fn provider_capabilities(url: &str) -> Option<ProviderCapabilities> {
    supports_capability_config(url).then(|| {
        capability_configs()
            .lock()
            .unwrap()
            .get(url.trim_end_matches('/'))
            .copied()
            .unwrap_or_default()
    })
}
pub fn configure_provider_capabilities(url: &str, config: ProviderCapabilities) {
    if supports_capability_config(url) {
        capability_configs()
            .lock()
            .unwrap()
            .insert(url.trim_end_matches('/').into(), config);
    }
}
/// Always start from the source manifest, so re-enabling a contribution cannot
/// be limited by an earlier filtered manifest cached on disk.
pub fn configured_provider_manifest(url: &str) -> Option<Manifest> {
    let config = provider_capabilities(url)?;
    let source = bundled_providers()
        .iter()
        .find(|p| p.url == url.trim_end_matches('/'))?;
    manifest_with_capabilities(source, config)
}
fn manifest_with_capabilities(
    source: &BundledProvider,
    config: ProviderCapabilities,
) -> Option<Manifest> {
    let manifest: PluginManifest = serde_json::from_str(source.plugin_manifest).ok()?;
    let addon: Value = serde_json::from_str(source.addon_manifest).ok()?;
    let runtime = PluginRuntime::new(manifest, source.source);
    let result = runtime
        .invoke(
            crate::MemoryProviderHost::new(),
            &json!({"op":"manifest", "manifest":addon, "config":config}),
        )
        .ok()?;
    serde_json::from_value(result).ok()
}

pub struct BundledProvider {
    pub url: &'static str,
    pub registration_key: &'static str,
    pub default_enabled: bool,
    pub plugin_manifest: &'static str,
    pub addon_manifest: &'static str,
    pub source: &'static str,
}

/// Only explicitly bundled assets resolve locally; arbitrary provider paths
/// cannot become filesystem reads or network redirects.
pub fn bundled_asset_bytes(url: &str) -> Option<&'static [u8]> {
    match url {
        "nova-provider://supernova/logo.jpg" => {
            Some(include_bytes!("../plugins/supernova/logo.jpg"))
        }
        _ => None,
    }
}

pub fn bundled_providers() -> &'static [BundledProvider] {
    static PROVIDERS: &[BundledProvider] = &[
        BundledProvider {
            url: ANIKOTO_PROVIDER_URL,
            registration_key: "providers:bundled:v1",
            default_enabled: true,
            plugin_manifest: include_str!("../plugins/anikoto/manifest.json"),
            addon_manifest: include_str!("../plugins/anikoto/stremio-manifest.json"),
            source: include_str!("../plugins/anikoto/index.js"),
        },
        BundledProvider {
            url: SUPERNOVA_PROVIDER_URL,
            registration_key: "providers:supernova:v1",
            default_enabled: false,
            plugin_manifest: include_str!("../plugins/supernova/manifest.json"),
            addon_manifest: include_str!("../plugins/supernova/stremio-manifest.json"),
            source: include_str!(concat!(env!("OUT_DIR"), "/supernova.js")),
        },
    ];
    PROVIDERS
}

pub fn provider_settings_schema(url: &str) -> Option<Vec<crate::plugin_settings::SettingField>> {
    let source = bundled_providers()
        .iter()
        .find(|p| p.url == url.trim_end_matches('/'))?;
    let manifest: PluginManifest = serde_json::from_str(source.plugin_manifest).ok()?;
    (!manifest.configuration.is_empty()).then_some(manifest.configuration)
}

pub fn supports_provider_configuration(url: &str) -> bool {
    supports_capability_config(url) || provider_settings_schema(url).is_some()
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
    if provider_capabilities(provider_url).is_some_and(|config| !config.streams) {
        return false;
    }
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
    let episode_context_valid = match request.media_type.as_str() {
        "series" => {
            request.season.is_some_and(|n| (1..=100).contains(&n))
                && request.episode.is_some_and(|n| (1..=10_000).contains(&n))
        }
        "movie" => {
            request.season.is_none()
                && request.episode.is_none()
                && request.absolute_episode.is_none()
                && request.season_episode_count.is_none()
                && request.series_episode_count.is_none()
                && request.episode_title.is_none()
        }
        _ => false,
    };
    if !episode_context_valid
        || request.media_id.is_empty()
        || request.title.trim().is_empty()
        || request.title.len() > 1024
        || request.media_id.len() > 1024
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
    fetch_builtin_addon_with_progress_and_timeout(raw_url, timeout, Arc::new(|_| {}))
}

/// Fetches a bundled response with validated complete stream snapshots. All
/// callbacks run on the invoking worker, never on the UI event loop.
pub fn fetch_builtin_addon_with_progress(
    raw_url: &str,
    on_progress: Arc<dyn Fn(Vec<u8>) + Send + Sync>,
) -> Option<Result<Vec<u8>, ProviderHostError>> {
    fetch_builtin_addon_with_progress_and_timeout(raw_url, Duration::from_secs(60), on_progress)
}

fn fetch_builtin_addon_with_progress_and_timeout(
    raw_url: &str,
    timeout: Duration,
    on_progress: Arc<dyn Fn(Vec<u8>) + Send + Sync>,
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
        return Some(
            serde_json::to_vec(
                &configured_provider_manifest(definition.url).unwrap_or_else(|| {
                    serde_json::from_str(definition.addon_manifest).expect("bundled manifest")
                }),
            )
            .map_err(|e| ProviderHostError(e.to_string())),
        );
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
        let config = provider_capabilities(definition.url).unwrap_or_default();
        match &request {
            ProtocolRequest::Catalog { .. } if !config.metadata => {
                return Ok(br#"{"metas":[]}"#.to_vec());
            }
            ProtocolRequest::Meta { .. } if !config.metadata => {
                return Ok(br#"{"meta":null}"#.to_vec());
            }
            ProtocolRequest::Streams { .. } | ProtocolRequest::Lookup { .. } if !config.streams => {
                return Ok(br#"{"streams":[]}"#.to_vec());
            }
            _ => {}
        }
        let state = crate::host::session_state();
        let host = Arc::new(ScopedHttpHost::new(
            state,
            manifest.permissions.domains.clone(),
            Duration::from_millis(manifest.limits.request_timeout_ms.min(60_000)),
            manifest.limits.response_bytes.min(RESPONSE_LIMIT),
            manifest.limits.redirects,
            manifest.limits.requests_per_operation,
        ));
        let mut provider = JsProvider::new(manifest, addon, definition.source);
        provider.config = config;
        let provider = Arc::new(provider);
        let host: Arc<dyn ProviderHost> = if matches!(
            &request,
            ProtocolRequest::Streams { .. } | ProtocolRequest::Lookup { .. }
        ) {
            Arc::new(StreamsPublisherHost {
                inner: host,
                provider: provider.clone(),
                on_progress: on_progress.clone(),
            })
        } else {
            host
        };
        let value = provider
            .protocol_response(host, request)
            .map_err(|e| ProviderHostError(e.to_string()))?;
        serde_json::to_vec(&value).map_err(|e| ProviderHostError(e.to_string()))
    };
    Some(execute())
}

struct StreamsPublisherHost {
    inner: Arc<dyn ProviderHost>,
    provider: Arc<JsProvider>,
    on_progress: Arc<dyn Fn(Vec<u8>) + Send + Sync>,
}
impl ProviderHost for StreamsPublisherHost {
    fn cancelled(&self) -> bool {
        self.inner.cancelled() || !self.provider.streams_configuration_current()
    }
    fn publish_streams(&self, raw: &str) -> Result<(), ProviderHostError> {
        if self.cancelled() {
            return Err(ProviderHostError(
                "provider stream publication cancelled".into(),
            ));
        }
        if raw.len() > RESPONSE_LIMIT {
            return Err(ProviderHostError(
                "provider streams exceeded their byte limit".into(),
            ));
        }
        let value = serde_json::from_str(raw)
            .map_err(|_| ProviderHostError("invalid provider stream snapshot".into()))?;
        let response = self
            .provider
            .streams_response(value)
            .map_err(|error| ProviderHostError(error.to_string()))?;
        let bytes = serde_json::to_vec(&response)
            .map_err(|_| ProviderHostError("invalid provider stream snapshot".into()))?;
        if self.cancelled() {
            return Err(ProviderHostError(
                "provider stream publication cancelled".into(),
            ));
        }
        (self.on_progress)(bytes);
        Ok(())
    }
    fn request(
        &self,
        request: &crate::HttpRequest,
    ) -> Result<crate::HttpResponse, ProviderHostError> {
        self.inner.request(request)
    }
    fn get(
        &self,
        url: &str,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> Result<crate::HttpResponse, ProviderHostError> {
        self.inner.get(url, headers)
    }
    fn storage_get(&self, key: &str) -> Option<String> {
        self.inner.storage_get(key)
    }
    fn storage_set(&self, key: &str, value: &str) -> Result<(), ProviderHostError> {
        self.inner.storage_set(key, value)
    }
    fn log(&self, message: &str) {
        self.inner.log(message)
    }
    fn host_work_elapsed(&self, duration: Duration) {
        self.inner.host_work_elapsed(duration)
    }
    fn metadata_available(&self, caller: &str) -> bool {
        self.inner.metadata_available(caller)
    }
    fn metadata_enrich(
        &self,
        request: crate::EnrichmentRequest,
        caller: &str,
        budget: Duration,
    ) -> crate::EnrichmentResult {
        self.inner.metadata_enrich(request, caller, budget)
    }
}

struct JsProvider {
    descriptor: ProviderDescriptor,
    runtime: PluginRuntime,
    config: ProviderCapabilities,
    settings: Result<crate::plugin_settings::SettingsSnapshot, String>,
    track_settings: bool,
    secret_keys: Vec<String>,
    playback_credentials: Vec<crate::runtime::EphemeralPlaybackCredential>,
}

impl JsProvider {
    fn new(manifest: PluginManifest, addon: Manifest, source: &str) -> Self {
        let settings =
            crate::plugin_settings::settings_snapshot(&manifest.id, &manifest.configuration)
                .map_err(|e| e.to_string());
        let track_settings = !manifest.configuration.is_empty();
        let secret_keys = manifest
            .configuration
            .iter()
            .filter(|field| field.kind == "secret")
            .map(|field| field.key.clone())
            .collect();
        Self {
            settings,
            track_settings,
            secret_keys,
            playback_credentials: manifest.permissions.ephemeral_playback_credentials.clone(),
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
            config: ProviderCapabilities::default(),
        }
    }
    fn call(
        &self,
        host: Arc<dyn ProviderHost>,
        mut request: Value,
    ) -> Result<Value, ProviderError> {
        let settings = self
            .settings
            .as_ref()
            .map_err(|error| ProviderError::Host(error.clone()))?;
        let current = || {
            !self.track_settings
                || settings.generation == crate::plugin_settings::settings_generation()
        };
        if !current() {
            return Err(ProviderError::Host("provider settings changed".into()));
        }
        request["config"] = serde_json::to_value(self.config).expect("capabilities serialize");
        request["settings"] = json!(settings.values);
        request["settingsGeneration"] = json!(settings.generation);
        let value = self.runtime.invoke(host, &request)?;
        if !current() {
            return Err(ProviderError::Host("provider settings changed".into()));
        }
        Ok(value)
    }
    fn contains_private_setting(&self, value: &str) -> bool {
        let Ok(settings) = &self.settings else {
            return false;
        };
        self.secret_keys
            .iter()
            .filter_map(|key| settings.values.get(key).and_then(Value::as_str))
            .flat_map(|secret| [secret, secret.trim()])
            .filter(|secret| !secret.is_empty())
            .any(|secret| {
                value.contains(secret)
                    || percent_decode_str(value)
                        .decode_utf8_lossy()
                        .contains(secret)
                    || percent_decode_str(&value.replace('+', " "))
                        .decode_utf8_lossy()
                        .contains(secret)
            })
    }
    fn value_contains_private_setting(&self, value: &Value) -> bool {
        match value {
            Value::String(value) => self.contains_private_setting(value),
            Value::Array(values) => values
                .iter()
                .any(|value| self.value_contains_private_setting(value)),
            Value::Object(fields) => fields.iter().any(|(key, value)| {
                self.contains_private_setting(key) || self.value_contains_private_setting(value)
            }),
            _ => false,
        }
    }
    fn sensitive_playback_url_allowed(&self, raw: &str) -> bool {
        let Ok(url) = Url::parse(raw) else {
            return false;
        };
        if url.scheme() != "https"
            || url.port().is_some_and(|port| port != 443)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return false;
        }
        let Some(url::Host::Domain(host)) = url.host() else {
            return false;
        };
        let host = host.to_ascii_lowercase();
        let Ok(settings) = &self.settings else {
            return false;
        };
        if self.playback_credentials.len() > 16 {
            return false;
        }
        self.playback_credentials.iter().any(|policy| {
            if !self.secret_keys.contains(&policy.setting)
                || policy.domains.is_empty()
                || policy.domains.len() > 16
                || policy.query_parameters.is_empty()
                || policy.query_parameters.len() > 16
                || !policy.path_prefix.starts_with('/')
                || !policy.path_prefix.ends_with('/')
                || policy.path_prefix.len() > 256
                || !url.path().starts_with(&policy.path_prefix)
            {
                return false;
            }
            if !policy.domains.iter().any(|domain| {
                if domain == "*" {
                    return true;
                }
                let domain = domain
                    .strip_prefix("*.")
                    .unwrap_or(domain)
                    .to_ascii_lowercase();
                !domain.is_empty()
                    && domain.len() <= 253
                    && !domain.contains(['/', ':', '@', '?', '#', '%'])
                    && (host == domain || host.ends_with(&format!(".{domain}")))
            }) {
                return false;
            }
            let Some(secret) = settings
                .values
                .get(&policy.setting)
                .and_then(Value::as_str)
                .filter(|secret| !secret.is_empty())
            else {
                return false;
            };
            let mut pairs: Vec<(String, String)> = url
                .query_pairs()
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect();
            let mut accepted = 0;
            for (key, value) in &mut pairs {
                if policy.query_parameters.contains(key) {
                    if value != secret && (secret.trim().is_empty() || value != secret.trim()) {
                        return false;
                    }
                    accepted += 1;
                    value.clear();
                }
            }
            if accepted != 1 {
                return false;
            }
            let mut public_url = url.clone();
            public_url.set_query(None);
            public_url.query_pairs_mut().extend_pairs(pairs);
            !self.contains_private_setting(public_url.as_str())
        })
    }
    fn resolve_stream(
        &self,
        host: Arc<dyn ProviderHost>,
        token: &str,
    ) -> Result<Value, ProviderResolveError> {
        if self.contains_private_setting(token) {
            return Err(ProviderResolveError::generic());
        }
        let value = self.call(host, json!({"op":"resolveStream", "token":token}))?;
        if let Some(error) = value.get("error") {
            let error: ProviderResolveError = serde_json::from_value(error.clone())
                .map_err(|_| ProviderResolveError::generic())?;
            return Err(
                if error.validate()
                    && !self.contains_private_setting(&error.message)
                    && !self.contains_private_setting(&error.code)
                {
                    error
                } else {
                    ProviderResolveError::generic()
                },
            );
        }
        if contains_sensitive_playback_flag(&value) {
            return Err(ProviderResolveError::generic());
        }
        let mut public_fields = value.clone();
        if let Some(fields) = public_fields.as_object_mut() {
            fields.remove("url");
        }
        if self.value_contains_private_setting(&public_fields) {
            return Err(ProviderResolveError::generic());
        }
        let stream: ProviderStream =
            serde_json::from_value(value).map_err(|_| ProviderResolveError::generic())?;
        let sensitive = self.contains_private_setting(&stream.url);
        if stream.resolver.is_some()
            || (sensitive && !self.sensitive_playback_url_allowed(&stream.url))
        {
            return Err(ProviderResolveError::generic());
        }
        let mut response =
            stream_to_stremio(&stream, &self.descriptor.id).map_err(ProviderResolveError::from)?;
        if sensitive {
            response["novaSensitivePlayback"] = json!(true);
        }
        Ok(response)
    }
    fn streams_configuration_current(&self) -> bool {
        let Ok(settings) = &self.settings else {
            return false;
        };
        (!self.track_settings
            || settings.generation == crate::plugin_settings::settings_generation())
            && provider_capabilities(&format!("nova-provider://{}", self.descriptor.id))
                .unwrap_or_default()
                == self.config
            && self.config.streams
    }
    fn streams_response(&self, result: Value) -> Result<Value, ProviderError> {
        let stream_rows = result
            .as_array()
            .or_else(|| result.get("streams").and_then(Value::as_array))
            .ok_or_else(|| {
                ProviderError::InvalidData("provider stream result was not a list".into())
            })?;
        if stream_rows.len() > 256 {
            return Err(ProviderError::InvalidData(
                "provider streams exceeded their row limit".into(),
            ));
        }
        let mut identities = std::collections::BTreeSet::new();
        let streams = stream_rows
            .iter()
            .map(|value| {
                if contains_sensitive_playback_flag(value)
                    || self.value_contains_private_setting(value)
                {
                    return Err(ProviderError::InvalidData(
                        "provider stream contains private settings".into(),
                    ));
                }
                let stream: ProviderStream = serde_json::from_value(value.clone())
                    .map_err(|error| ProviderError::InvalidData(error.to_string()))?;
                if stream.id.is_empty()
                    || stream.id.len() > 8192
                    || !identities.insert(stream.id.clone())
                {
                    return Err(ProviderError::InvalidData(
                        "invalid or duplicate provider stream identity".into(),
                    ));
                }
                stream_to_stremio(&stream, &self.descriptor.id)
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;
        let mut response = json!({"streams":streams});
        if let Some(cache_hit) = result.get("cacheHit") {
            let cache_hit = cache_hit.as_bool().ok_or_else(|| {
                ProviderError::InvalidData("invalid provider cache origin".into())
            })?;
            if cache_hit {
                response["novaCacheHit"] = json!(true);
            }
        }
        if let Some(warnings) = result.get("warnings") {
            let warnings = warnings
                .as_array()
                .filter(|warnings| warnings.len() <= 16)
                .ok_or_else(|| ProviderError::InvalidData("invalid provider warnings".into()))?;
            let warnings = warnings
                .iter()
                .map(|warning| {
                    let message = warning
                        .as_str()
                        .or_else(|| warning.get("message").and_then(Value::as_str))
                        .ok_or_else(|| {
                            ProviderError::InvalidData("invalid provider warning".into())
                        })?;
                    if warning
                        .get("source")
                        .is_some_and(|source| !source.is_null() && !source.is_string())
                    {
                        return Err(ProviderError::InvalidData(
                            "invalid provider warning source".into(),
                        ));
                    }
                    let source = warning.get("source").and_then(Value::as_str);
                    if message.trim().is_empty()
                        || message.len() > 256
                        || message.chars().any(char::is_control)
                        || source.is_some_and(|source| {
                            source.len() > 64 || source.chars().any(char::is_control)
                        })
                    {
                        return Err(ProviderError::InvalidData(
                            "invalid provider warning".into(),
                        ));
                    }
                    if self.contains_private_setting(message)
                        || source.is_some_and(|source| self.contains_private_setting(source))
                    {
                        return Ok(json!({"message":"A provider source is unavailable."}));
                    }
                    Ok(match source {
                        Some(source) => json!({"source":source,"message":message}),
                        None => json!({"message":message}),
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?;
            if !warnings.is_empty() {
                response["novaWarnings"] = json!(warnings);
            }
        }
        if serde_json::to_vec(&response)
            .map_err(|error| ProviderError::InvalidData(error.to_string()))?
            .len()
            > RESPONSE_LIMIT
        {
            return Err(ProviderError::InvalidData(
                "provider streams exceeded their byte limit".into(),
            ));
        }
        Ok(response)
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
                        if contains_sensitive_playback_flag(value)
                            || self.value_contains_private_setting(value)
                        {
                            return Err(ProviderError::InvalidData(
                                "provider catalog contains private settings".into(),
                            ));
                        }
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
                if contains_sensitive_playback_flag(&result)
                    || self.value_contains_private_setting(&result)
                {
                    return Err(ProviderError::InvalidData(
                        "provider details contain private settings".into(),
                    ));
                }
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
                self.streams_response(result)
            }
        }
    }
}

fn contains_sensitive_playback_flag(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(key, value)| {
            key == "novaSensitivePlayback"
                || key == "nova_sensitive_playback"
                || contains_sensitive_playback_flag(value)
        }),
        Value::Array(values) => values.iter().any(contains_sensitive_playback_flag),
        _ => false,
    }
}

fn stream_to_stremio(stream: &ProviderStream, provider_id: &str) -> Result<Value, ProviderError> {
    stream.validate(provider_id)?;
    let mut result = json!({
        "novaProviderStreamId": format!("{provider_id}:{}",stream.id),
        "name": stream.title,
        "description": stream.description,
        "subtitles": stream.subtitles.iter().enumerate().map(|(index, subtitle)| json!({
            "id":index.to_string(), "url":subtitle.url, "lang":subtitle.language,
        })).collect::<Vec<_>>(),
        "behaviorHints":{"proxyHeaders":{"request":stream.headers}},
    });
    if !stream.url.is_empty() {
        result["url"] = json!(stream.url);
    }
    if let Some(hash) = &stream.info_hash {
        result["infoHash"] = json!(hash.to_ascii_lowercase());
    }
    if let Some(index) = stream.file_idx {
        result["fileIdx"] = json!(index);
    }
    if let Some(resolver) = &stream.resolver {
        result["novaResolver"] = json!({"providerId":resolver.provider_id, "token":resolver.token});
    }
    if let Some(size) = stream.size {
        result["size"] = json!(size);
    }
    if let Some(seeders) = stream.seeders {
        result["seeders"] = json!(seeders);
    }
    if let Some(source) = &stream.source {
        result["source"] = json!(source);
    }
    Ok(result)
}

/// Stable UI identity for one owner's request, independent of display labels,
/// reported peers, response order and partial/final snapshot timing. Private
/// playback targets are hashed, never embedded in a UI key.
pub fn stable_stream_identity(
    owner_url: &str,
    request_id: &str,
    stream: &addons::Stream,
) -> String {
    let owner = owner_url.trim_end_matches('/');
    let owned_id = bundled_providers()
        .iter()
        .find(|definition| definition.url == owner)
        .and_then(|definition| Url::parse(definition.url).ok())
        .and_then(|url| url.host_str().map(str::to_owned))
        .and_then(|provider| {
            stream
                .extra
                .get("novaProviderStreamId")
                .and_then(Value::as_str)
                .filter(|identity| {
                    identity.len() <= 8257
                        && identity
                            .strip_prefix(&format!("{provider}:"))
                            .is_some_and(|id| !id.is_empty())
                })
                .map(str::to_owned)
        });
    let identity = if let Some(id) = owned_id {
        json!({"owner":owner,"request":request_id,"providerStream":id})
    } else {
        let headers = stream
            .extra
            .get("behaviorHints")
            .and_then(|hints| hints.get("proxyHeaders"))
            .and_then(|headers| headers.get("request"))
            .and_then(Value::as_object)
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|(name, value)| {
                        value
                            .as_str()
                            .map(|value| (name.to_ascii_lowercase(), value.to_owned()))
                    })
                    .collect::<std::collections::BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        json!({"owner":owner,"request":request_id,"url":stream.url,"youtube":stream.yt_id,
            "infoHash":stream.info_hash.as_ref().map(|hash|hash.trim().to_ascii_lowercase()),"fileIdx":stream.file_idx,
            "headers":headers,"subtitles":stream.subtitles.iter().map(|subtitle|json!({"url":subtitle.url,"language":subtitle.lang})).collect::<Vec<_>>()})
    };
    let data = serde_json::to_vec(&identity).expect("stream identity serializes");
    let hash = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &data);
    let hex: String = hash
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("stream-{hex}")
}

/// Resolves a deferred token only inside its registered owner's sandbox. The
/// caller supplies cancellation tied to the current UI request generation.
pub fn resolve_provider_stream(
    provider_url: &str,
    token: &str,
    timeout: Duration,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
) -> Result<addons::Stream, ProviderResolveError> {
    if cancelled() {
        return Err(ProviderResolveError::cancelled());
    }
    let definition = bundled_providers()
        .iter()
        .find(|p| p.url == provider_url.trim_end_matches('/'))
        .ok_or_else(ProviderResolveError::generic)?;
    if token.is_empty() || token.len() > 8192 {
        return Err(ProviderResolveError::generic());
    }
    let config = provider_capabilities(definition.url).unwrap_or_default();
    if !config.streams {
        return Err(ProviderResolveError::generic());
    }
    let mut manifest: PluginManifest = serde_json::from_str(definition.plugin_manifest)
        .map_err(|_| ProviderResolveError::generic())?;
    manifest.limits.request_timeout_ms = manifest
        .limits
        .resolver_timeout_ms
        .min(timeout.as_millis().clamp(1, 60_000) as u64);
    let addon = serde_json::from_str(definition.addon_manifest)
        .map_err(|_| ProviderResolveError::generic())?;
    let host = Arc::new(
        ScopedHttpHost::new(
            crate::host::session_state(),
            manifest.permissions.domains.clone(),
            Duration::from_millis(manifest.limits.request_timeout_ms),
            manifest.limits.response_bytes.min(RESPONSE_LIMIT),
            manifest.limits.redirects,
            manifest.limits.requests_per_operation,
        )
        .with_cancellation(cancelled.clone()),
    );
    let mut provider = JsProvider::new(manifest, addon, definition.source);
    provider.config = config;
    let result = provider.resolve_stream(host, token);
    if cancelled() {
        return Err(ProviderResolveError::cancelled());
    }
    let value = result?;
    serde_json::from_value(value).map_err(|_| ProviderResolveError::generic())
}

/// The opt-in live test supplies an immutable in-memory credential snapshot;
/// validation follows the production final-only path without saving settings.
#[cfg(test)]
pub(crate) fn resolve_with_snapshot_for_test(
    host: Arc<dyn ProviderHost>,
    token: &str,
    values: serde_json::Map<String, Value>,
) -> Result<Value, ProviderResolveError> {
    let definition = bundled_providers()
        .iter()
        .find(|provider| provider.url == SUPERNOVA_PROVIDER_URL)
        .ok_or_else(ProviderResolveError::generic)?;
    let mut manifest: PluginManifest = serde_json::from_str(definition.plugin_manifest)
        .map_err(|_| ProviderResolveError::generic())?;
    manifest.limits.request_timeout_ms = manifest.limits.resolver_timeout_ms;
    let addon: Manifest = serde_json::from_str(definition.addon_manifest)
        .map_err(|_| ProviderResolveError::generic())?;
    let mut provider = JsProvider::new(manifest, addon, definition.source);
    provider.settings = Ok(crate::plugin_settings::SettingsSnapshot {
        values,
        generation: crate::plugin_settings::settings_generation(),
    });
    provider.track_settings = false;
    provider.resolve_stream(host, token)
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
            let rows = value.get("streams").cloned().unwrap_or(value);
            let streams: Vec<ProviderStream> = serde_json::from_value(rows)
                .map_err(|error| ProviderError::InvalidData(error.to_string()))?;
            for stream in &streams {
                stream.validate(&self.descriptor.id)?;
            }
            Ok(streams)
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
        Some("resolve")
            if segments.len() == 3 && matches!(segments[1].as_str(), "movie" | "series") =>
        {
            let data = segments[2].strip_suffix(".json").unwrap_or(&segments[2]);
            if data.len() > 8192 {
                return Err(ProviderHostError(
                    "source lookup exceeded its byte limit".into(),
                ));
            }
            let request: StreamLookupRequest =
                serde_json::from_str(data).map_err(|error| ProviderHostError(error.to_string()))?;
            validate_stream_lookup(&request)?;
            if request.media_type != segments[1] {
                return Err(ProviderHostError(
                    "source lookup media type mismatch".into(),
                ));
            }
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

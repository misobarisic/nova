use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use base64::Engine;
use rquickjs::{Context, Function, Object, Runtime};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};

use crate::host::{HttpRequest, HttpResponseType};
use crate::{ProviderError, ProviderHost};

const MAX_PLUGIN_MEMORY: usize = 32 * 1024 * 1024;
const MAX_PLUGIN_STACK: usize = 512 * 1024;
const DEFAULT_PLUGIN_RUN_MS: u64 = 5_000;
const MAX_PLUGIN_RUN_MS: u64 = 10_000;
const MAX_PLUGIN_BODY: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub format_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub entry: String,
    #[serde(default)]
    pub scripts: Vec<String>,
    #[serde(default)]
    pub assets: Vec<String>,
    pub permissions: PluginPermissions,
    #[serde(default)]
    pub capabilities: PluginCapabilities,
    #[serde(default)]
    pub limits: PluginLimits,
    #[serde(default)]
    pub configuration: Vec<crate::plugin_settings::SettingField>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PluginPermissions {
    #[serde(default, rename = "addonMetadata")]
    pub addon_metadata: bool,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default, rename = "httpPost")]
    pub http_post: bool,
    #[serde(default, rename = "ephemeralPlaybackCredentials")]
    pub ephemeral_playback_credentials: Vec<EphemeralPlaybackCredential>,
}

/// A final deferred playback URL may carry one configured secret only in a
/// declared query parameter on a trusted HTTPS service path. This permission
/// never authorizes secrets in catalog rows, caches, headers or diagnostics.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EphemeralPlaybackCredential {
    pub setting: String,
    pub domains: Vec<String>,
    pub query_parameters: Vec<String>,
    pub path_prefix: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCapabilities {
    #[serde(default)]
    pub contextual_streams: bool,
    #[serde(default)]
    pub configurable_capabilities: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PluginLimits {
    pub script_time_ms: u64,
    pub request_timeout_ms: u64,
    pub resolver_timeout_ms: u64,
    pub response_bytes: usize,
    pub redirects: usize,
    pub requests_per_operation: usize,
}

impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            script_time_ms: DEFAULT_PLUGIN_RUN_MS,
            request_timeout_ms: 15_000,
            resolver_timeout_ms: 30_000,
            response_bytes: MAX_PLUGIN_BODY,
            redirects: 5,
            requests_per_operation: 12,
        }
    }
}

impl PluginLimits {
    fn script_budget(&self) -> Duration {
        Duration::from_millis(self.script_time_ms.clamp(1, MAX_PLUGIN_RUN_MS))
    }
}

/// A fresh QuickJS heap is created for every provider operation. The only
/// injected capabilities are host-checked HTTP, CSS selection, namespaced
/// storage, and logging. No module loader is registered, so JS imports cannot
/// reach native or remote modules.
pub struct PluginRuntime {
    manifest: PluginManifest,
    source: String,
}

impl PluginRuntime {
    pub fn new(manifest: PluginManifest, source: impl Into<String>) -> Self {
        Self {
            manifest,
            source: source.into(),
        }
    }

    pub fn invoke(
        &self,
        host: Arc<dyn ProviderHost>,
        request: &serde_json::Value,
    ) -> Result<serde_json::Value, ProviderError> {
        if host.cancelled() {
            return Err(ProviderError::InvalidData(
                "provider operation cancelled".into(),
            ));
        }
        if self.manifest.format_version != 1
            || self.manifest.id.is_empty()
            || self.manifest.id.len() > 64
            || !self
                .manifest
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(ProviderError::InvalidData(
                "invalid provider manifest version or ID".into(),
            ));
        }
        let streams_allowed = matches!(
            request.get("op").and_then(serde_json::Value::as_str),
            Some("lookupStreams" | "streams")
        );
        let is_details = request.get("op").and_then(serde_json::Value::as_str) == Some("details");
        let fallback = Arc::new(Mutex::new(None::<String>));
        let (settings_snapshot, settings_generation) = if let Some(values) = request
            .get("settings")
            .and_then(serde_json::Value::as_object)
        {
            (
                Some(values.clone()),
                request
                    .get("settingsGeneration")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_else(crate::plugin_settings::settings_generation),
            )
        } else {
            let snapshot = crate::plugin_settings::settings_snapshot(
                &self.manifest.id,
                &self.manifest.configuration,
            )
            .map_err(|_| ProviderError::InvalidData("invalid provider configuration".into()))?;
            (Some(snapshot.values), snapshot.generation)
        };
        if self.manifest.configuration.len() > 128
            || settings_snapshot.as_ref().is_some_and(|values| {
                values.len() > 128
                    || serde_json::to_string(values).map_or(true, |raw| raw.len() > 256 * 1024)
            })
        {
            return Err(ProviderError::InvalidData(
                "provider settings snapshot exceeded its limit".into(),
            ));
        }
        let request = serde_json::to_string(request)
            .map_err(|error| ProviderError::InvalidData(error.to_string()))?;
        if request.len() > MAX_PLUGIN_BODY || self.source.len() > MAX_PLUGIN_BODY {
            return Err(ProviderError::InvalidData(
                "provider input exceeded its byte limit".into(),
            ));
        }
        let sensitive_settings = settings_snapshot.clone().unwrap_or_else(|| {
            crate::plugin_settings::settings_snapshot(
                &self.manifest.id,
                &self.manifest.configuration,
            )
            .map(|snapshot| snapshot.values)
            .unwrap_or_default()
        });
        let secrets: Vec<String> = self
            .manifest
            .configuration
            .iter()
            .filter(|field| field.kind == "secret")
            .filter_map(|field| {
                sensitive_settings
                    .get(&field.key)
                    .and_then(serde_json::Value::as_str)
            })
            .flat_map(|value| [value, value.trim()])
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect();
        let budget = Arc::new(ExecutionBudget {
            started: Instant::now(),
            script_limit: self.manifest.limits.script_budget(),
            wall_limit: Duration::from_millis(
                self.manifest.limits.request_timeout_ms.clamp(1, 60_000),
            ),
            http_micros: AtomicU64::new(0),
            host_intervals: Mutex::new(Vec::new()),
        });
        let host = Arc::new(OperationHost {
            inner: host,
            secrets,
            provider_id: self.manifest.id.clone(),
            budget: budget.clone(),
            request_limit: self.manifest.limits.requests_per_operation.min(64),
            response_bytes: self.manifest.limits.response_bytes.min(MAX_PLUGIN_BODY),
            requests: AtomicUsize::new(0),
            total_response_bytes: AtomicUsize::new(0),
            storage_writes: AtomicUsize::new(0),
            log_calls: AtomicUsize::new(0),
            stream_snapshots: AtomicUsize::new(0),
            streams_allowed,
            has_configuration: !self.manifest.configuration.is_empty(),
            settings_generation,
        });
        let runtime = Runtime::new().map_err(script_error)?;
        runtime.set_memory_limit(MAX_PLUGIN_MEMORY);
        runtime.set_max_stack_size(MAX_PLUGIN_STACK);
        let cancellation_host = host.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || {
            budget.expired() || cancellation_host.cancelled()
        })));
        let context = Context::full(&runtime).map_err(script_error)?;
        let raw = context.with(|ctx| -> Result<String, ProviderError> {
            let convert = |error| {
                ProviderError::Script(
                    host.redact(&rquickjs::CaughtError::from_error(&ctx, error).to_string()),
                )
            };
            install_capabilities(
                &ctx,
                host.clone(),
                &self.manifest,
                fallback.clone(),
                is_details,
                settings_snapshot,
                settings_generation,
            )
            .map_err(convert)?;
            ctx.eval::<(), _>(self.source.as_str()).map_err(convert)?;
            ctx.globals()
                .set("__nova_request", request)
                .map_err(convert)?;
            ctx.eval::<String, _>(
                "JSON.stringify(globalThis.novaProvider.handle(JSON.parse(__nova_request)))",
            )
            .map_err(convert)
        });
        if host.cancelled() {
            return Err(ProviderError::InvalidData(
                "provider operation cancelled".into(),
            ));
        }
        let raw = if host.budget.expired() {
            Err(ProviderError::Script(
                "provider exceeded its operation time limit".into(),
            ))
        } else {
            raw
        };
        let mut raw = match raw {
            Ok(raw) => raw,
            Err(error) => {
                // Optional discovery/enrichment must not discard details already
                // fetched. Never resume interrupted JS to retrieve its fallback.
                let Some(native) = fallback.lock().unwrap().take() else {
                    return Err(error);
                };
                host.log(&format!("optional metadata work failed: {error}"));
                native
            }
        };
        if raw.len() > MAX_PLUGIN_BODY
            && let Some(native) = fallback.lock().unwrap().take()
        {
            raw = native;
        }
        if raw.len() > MAX_PLUGIN_BODY {
            return Err(ProviderError::InvalidData(
                "provider output exceeded its byte limit".into(),
            ));
        }
        serde_json::from_str(&raw).map_err(|error| ProviderError::Script(error.to_string()))
    }
}

struct OperationHost {
    inner: Arc<dyn ProviderHost>,
    secrets: Vec<String>,
    provider_id: String,
    budget: Arc<ExecutionBudget>,
    request_limit: usize,
    response_bytes: usize,
    requests: AtomicUsize,
    total_response_bytes: AtomicUsize,
    storage_writes: AtomicUsize,
    log_calls: AtomicUsize,
    stream_snapshots: AtomicUsize,
    streams_allowed: bool,
    has_configuration: bool,
    settings_generation: u64,
}

impl OperationHost {
    fn redact(&self, message: &str) -> String {
        let mut message = message.to_owned();
        for secret in &self.secrets {
            message = message.replace(secret, "[redacted]");
        }
        crate::host::sanitize_log(&message)
    }
}

struct ExecutionBudget {
    started: Instant,
    script_limit: Duration,
    wall_limit: Duration,
    http_micros: AtomicU64,
    host_intervals: Mutex<Vec<(Instant, Instant)>>,
}

impl ExecutionBudget {
    fn expired(&self) -> bool {
        let elapsed = self.started.elapsed();
        // Blocking host IO consumes the overall operation deadline, but does
        // not spend the script's CPU budget while it waits for the network.
        elapsed >= self.wall_limit
            || elapsed.saturating_sub(Duration::from_micros(
                self.http_micros.load(Ordering::Relaxed),
            )) >= self.script_limit
    }
}

impl ProviderHost for OperationHost {
    fn get(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<crate::HttpResponse, crate::ProviderHostError> {
        self.request(&HttpRequest::get(url, headers))
    }

    fn cancelled(&self) -> bool {
        self.inner.cancelled()
    }

    fn request(
        &self,
        request: &HttpRequest,
    ) -> Result<crate::HttpResponse, crate::ProviderHostError> {
        request.validate()?;
        if self.cancelled() {
            return Err(crate::ProviderHostError(
                "provider operation cancelled".into(),
            ));
        }
        let count = self.requests.fetch_add(1, Ordering::Relaxed) + 1;
        if count > self.request_limit {
            return Err(crate::ProviderHostError(
                "provider exceeded its per-operation request limit".into(),
            ));
        }
        if self.budget.expired() {
            return Err(crate::ProviderHostError(
                "provider exceeded its operation time limit".into(),
            ));
        }
        let response = self.inner.request(request);
        // Workers never credit IO time: JS may be executing a completion
        // callback while another request is in flight. Only the invoking
        // thread records its actual blocking waits in the capability below.
        if self.cancelled() || self.budget.started.elapsed() >= self.budget.wall_limit {
            return Err(crate::ProviderHostError(
                "provider operation cancelled or timed out".into(),
            ));
        }
        let response =
            response.map_err(|error| crate::ProviderHostError(self.redact(&error.to_string())))?;
        if response.body.len() > self.response_bytes
            || self
                .total_response_bytes
                .fetch_add(response.body.len(), Ordering::Relaxed)
                .saturating_add(response.body.len())
                > self.response_bytes.saturating_mul(4)
        {
            return Err(crate::ProviderHostError(
                "provider response exceeded its byte limit".into(),
            ));
        }
        Ok(response)
    }

    fn publish_streams(&self, rows: &str) -> Result<(), crate::ProviderHostError> {
        if !self.streams_allowed
            || self.cancelled()
            || self.budget.expired()
            || (self.has_configuration
                && crate::plugin_settings::settings_generation() != self.settings_generation)
        {
            return Err(crate::ProviderHostError(
                "stream publication unavailable or stale".into(),
            ));
        }
        if rows.len() > MAX_PLUGIN_BODY
            || self.stream_snapshots.fetch_add(1, Ordering::Relaxed) >= 16
        {
            return Err(crate::ProviderHostError(
                "stream publication exceeded its limit".into(),
            ));
        }
        let value: serde_json::Value = serde_json::from_str(rows)
            .map_err(|_| crate::ProviderHostError("invalid stream publication".into()))?;
        let stream_rows = value
            .as_array()
            .or_else(|| value.get("streams").and_then(serde_json::Value::as_array));
        let warnings_valid = value.get("warnings").is_none_or(|warnings| {
            warnings
                .as_array()
                .is_some_and(|warnings| warnings.len() <= 16)
        });
        if !stream_rows.is_some_and(|rows| rows.len() <= 256) || !warnings_valid {
            return Err(crate::ProviderHostError(
                "invalid stream publication".into(),
            ));
        }
        self.inner
            .publish_streams(rows)
            .map_err(|_| crate::ProviderHostError("stream publication rejected".into()))
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
        let started = Instant::now();
        let remaining = self
            .budget
            .wall_limit
            .saturating_sub(self.budget.started.elapsed());
        let result = self
            .inner
            .metadata_enrich(request, caller, budget.min(remaining));
        self.host_work_elapsed(started.elapsed());
        result
    }

    fn host_work_elapsed(&self, duration: Duration) {
        let end = Instant::now();
        let start = end.checked_sub(duration).unwrap_or(self.budget.started);
        if let Ok(mut intervals) = self.budget.host_intervals.lock() {
            intervals.push((start, end));
            intervals.sort_unstable_by_key(|interval| interval.0);
            let mut merged: Vec<(Instant, Instant)> = Vec::new();
            for &(start, end) in intervals.iter() {
                if let Some(last) = merged.last_mut()
                    && start <= last.1
                {
                    last.1 = last.1.max(end);
                } else {
                    merged.push((start, end));
                }
            }
            let micros: u128 = merged
                .iter()
                .map(|(start, end)| end.duration_since(*start).as_micros())
                .sum();
            *intervals = merged;
            self.budget
                .http_micros
                .store(micros.min(u64::MAX as u128) as u64, Ordering::Relaxed);
        }
    }

    fn storage_get(&self, key: &str) -> Option<String> {
        if key.len() > 128 {
            return None;
        }
        self.inner
            .storage_get(&format!("{}:{key}", self.provider_id))
            .filter(|value| value.len() <= 16 * 1024)
    }

    fn storage_set(&self, key: &str, value: &str) -> Result<(), crate::ProviderHostError> {
        if key.len() > 128 || value.len() > 16 * 1024 {
            return Err(crate::ProviderHostError(
                "provider storage entry is too large".into(),
            ));
        }
        if self.storage_writes.fetch_add(1, Ordering::Relaxed) >= 128 {
            return Err(crate::ProviderHostError(
                "provider exceeded its storage write limit".into(),
            ));
        }
        self.inner
            .storage_set(&format!("{}:{key}", self.provider_id), value)
    }

    fn log(&self, message: &str) {
        if self.log_calls.fetch_add(1, Ordering::Relaxed) < 20 {
            self.inner
                .log(&format!("{}: {}", self.provider_id, self.redact(message)));
        }
    }
}

fn install_capabilities<'js>(
    ctx: &rquickjs::Ctx<'js>,
    host: Arc<dyn ProviderHost>,
    manifest: &PluginManifest,
    fallback: Arc<Mutex<Option<String>>>,
    is_details: bool,
    settings_snapshot: Option<serde_json::Map<String, serde_json::Value>>,
    settings_generation: u64,
) -> rquickjs::Result<()> {
    let nova = Object::new(ctx.clone())?;
    install_settings(
        ctx,
        &nova,
        manifest,
        settings_snapshot,
        settings_generation,
        host.clone(),
    )?;
    let http = Object::new(ctx.clone())?;
    let html = Object::new(ctx.clone())?;
    let crypto = Object::new(ctx.clone())?;
    let storage = Object::new(ctx.clone())?;

    let host_ref = host.clone();
    let permissions = manifest.permissions.clone();
    http.set(
        "get",
        Function::new(ctx.clone(), move |url: String, headers_json: String| {
            if url.len() > 8192 || headers_json.len() > 16 * 1024 {
                return serde_json::json!({"error":"provider HTTP arguments exceeded their byte limit"}).to_string();
            }
            let Ok(headers) = serde_json::from_str::<BTreeMap<String, String>>(&headers_json) else {
                return serde_json::json!({"error":"invalid provider HTTP headers"}).to_string();
            };
            if headers.len() > 16 || headers.iter().map(|(name, value)| name.len() + value.len()).sum::<usize>() > 8 * 1024 {
                return serde_json::json!({"error":"provider HTTP headers exceeded their byte limit"}).to_string();
            }
            if !plugin_url_allowed(&url, &permissions.domains) {
                return serde_json::json!({"error":"URL is outside the provider's domain permissions"}).to_string();
            }
            let started = Instant::now();
            let response = host_ref.get(&url, &headers);
            host_ref.host_work_elapsed(started.elapsed());
            match response {
                Ok(response) if response.body.len() <= MAX_PLUGIN_BODY => {
                    let body = String::from_utf8_lossy(&response.body).into_owned();
                    serde_json::json!({
                        "status": response.status,
                        "url": response.final_url,
                        "contentType": response.content_type,
                        "body": body,
                    })
                    .to_string()
                }
                Ok(_) => serde_json::json!({"error":"provider response exceeded the host size limit"}).to_string(),
                Err(error) => serde_json::json!({"error":crate::host::sanitize_log(&error.to_string())}).to_string(),
            }
        })?,
    )?;

    let host_ref = host.clone();
    let domains = manifest.permissions.domains.clone();
    let http_post = manifest.permissions.http_post;
    http.set(
        "request",
        Function::new(ctx.clone(), move |raw: String| {
            match parse_http_request(&raw, &domains) {
                Ok(request) if request.method == "POST" && !http_post => {
                    serde_json::json!({"error":"provider POST permission denied"}).to_string()
                }
                Ok(request) => {
                    let started = Instant::now();
                    let response = host_ref.request(&request);
                    host_ref.host_work_elapsed(started.elapsed());
                    http_result(response, request.response_type).to_string()
                }
                Err(error) => serde_json::json!({"error":error}).to_string(),
            }
        })?,
    )?;
    let host_ref = host.clone();
    let domains = manifest.permissions.domains.clone();
    http.set("batch", Function::new(ctx.clone(), move |raw: String| {
        if raw.len() > MAX_PLUGIN_BODY { return serde_json::json!({"error":"HTTP batch is too large"}).to_string(); }
        let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&raw) else { return serde_json::json!({"error":"invalid HTTP batch"}).to_string(); };
        if items.len() > 3 { return serde_json::json!({"error":"HTTP batch permits at most three GET requests"}).to_string(); }
        let started = Instant::now();
        let mut results = std::thread::scope(|scope| {
            let tasks: Vec<_> = items.into_iter().map(|item| {
                let host = &host_ref;
                let domains = &domains;
                scope.spawn(move || {
                    match parse_http_request(&item.to_string(), domains) {
                        Ok(request) if request.method == "GET" => http_result(host.request(&request), request.response_type),
                        Ok(_) => serde_json::json!({"error":"HTTP batch supports GET only"}),
                        Err(error) => serde_json::json!({"error":error}),
                    }
                })
            }).collect();
            tasks.into_iter().map(|task| task.join().unwrap_or_else(|_| serde_json::json!({"error":"HTTP batch request failed"}))).collect::<Vec<_>>()
        });
        host_ref.host_work_elapsed(started.elapsed());
        let mut output_bytes = 2usize;
        for item in &mut results {
            let size = item.to_string().len() + 1;
            if output_bytes.saturating_add(size) > MAX_PLUGIN_BODY {
                *item = serde_json::json!({"error":"HTTP batch exceeded its aggregate output limit"});
            }
            output_bytes += item.to_string().len() + 1;
        }
        serde_json::to_string(&results).unwrap_or_else(|_| "[]".into())
    })?)?;
    let host_ref = host.clone();
    let domains = manifest.permissions.domains.clone();
    http.set("batchEach", Function::new(ctx.clone(), move |raw: String, callback: Function<'js>| -> rquickjs::Result<String> {
        if raw.len() > MAX_PLUGIN_BODY { return Ok(serde_json::json!({"error":"HTTP batch is too large"}).to_string()); }
        let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&raw) else { return Ok(serde_json::json!({"error":"invalid HTTP batch"}).to_string()); };
        if items.len() > 3 { return Ok(serde_json::json!({"error":"HTTP batch permits at most three GET requests"}).to_string()); }
        std::thread::scope(|scope| {
            let (sender, receiver) = std::sync::mpsc::channel();
            let count = items.len();
            for (index,item) in items.into_iter().enumerate() {
                let sender = sender.clone();
                let host = &host_ref;
                let domains = &domains;
                scope.spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match parse_http_request(&item.to_string(),domains) {
                        Ok(request) if request.method == "GET" => http_result(host.request(&request),request.response_type),
                        Ok(_) => serde_json::json!({"error":"HTTP batch supports GET only"}),
                        Err(error) => serde_json::json!({"error":error}),
                    })).unwrap_or_else(|_| serde_json::json!({"error":"HTTP batch request failed"}));
                    let _ = sender.send((index,result));
                });
            }
            drop(sender);
            let mut results = vec![serde_json::Value::Null;count];
            let mut output_bytes = 2usize;
            for _ in 0..count {
                let started = Instant::now();
                let received = receiver.recv();
                host_ref.host_work_elapsed(started.elapsed());
                let Ok((index,mut response)) = received else { break; };
                if output_bytes.saturating_add(response.to_string().len()+1) > MAX_PLUGIN_BODY {
                    response = serde_json::json!({"error":"HTTP batch exceeded its aggregate output limit"});
                }
                output_bytes += response.to_string().len()+1;
                let raw = response.to_string();
                results[index] = response;
                if !host_ref.cancelled() { callback.call::<_,()>((index,raw))?; }
            }
            Ok(serde_json::to_string(&results).unwrap_or_else(|_| "[]".into()))
        })
    })?)?;
    let streams = Object::new(ctx.clone())?;
    let host_ref = host.clone();
    streams.set(
        "publish",
        Function::new(ctx.clone(), move |rows: String| {
            host_ref.publish_streams(&rows).map_or_else(
                |error| crate::host::sanitize_log(&error.to_string()),
                |()| String::new(),
            )
        })?,
    )?;
    nova.set("streams", streams)?;
    let torrent = Object::new(ctx.clone())?;
    torrent.set(
        "inspect",
        Function::new(ctx.clone(), |encoded: String| {
            if encoded.len() > MAX_PLUGIN_BODY.div_ceil(3) * 4 {
                return serde_json::json!({"error":"torrent metadata is too large"}).to_string();
            }
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
                return serde_json::json!({"error":"invalid torrent encoding"}).to_string();
            };
            match crate::inspect_torrent_metadata(&bytes) {
                Ok(metadata) => serde_json::to_string(&metadata).unwrap_or_else(|_| "{}".into()),
                Err(_) => serde_json::json!({"error":"invalid torrent metadata"}).to_string(),
            }
        })?,
    )?;
    nova.set("torrent", torrent)?;
    let xml = Object::new(ctx.clone())?;
    xml.set(
        "select",
        Function::new(ctx.clone(), |raw: String, query: String, fields: String| {
            select_xml(&raw, &query, &fields)
        })?,
    )?;
    nova.set("xml", xml)?;

    html.set(
        "select",
        Function::new(
            ctx.clone(),
            |raw_html: String, query: String, fields_json: String| {
                select_html(&raw_html, &query, &fields_json)
            },
        )?,
    )?;

    let host_ref = host.clone();
    storage.set(
        "get",
        Function::new(ctx.clone(), move |key: String| {
            host_ref.storage_get(&key).unwrap_or_default()
        })?,
    )?;
    let host_ref = host.clone();
    storage.set(
        "set",
        Function::new(ctx.clone(), move |key: String, value: String| {
            host_ref
                .storage_set(&key, &value)
                .map(|()| String::new())
                .unwrap_or_else(|error| error.to_string())
        })?,
    )?;
    let metadata = Object::new(ctx.clone())?;
    let provider_id = manifest.id.clone();
    metadata.set(
        "checkpoint",
        Function::new(ctx.clone(), move |raw: String| {
            if !is_details || raw.len() > MAX_PLUGIN_BODY {
                return "invalid native details checkpoint".to_owned();
            }
            let Ok(details) = serde_json::from_str::<crate::ProviderDetails>(&raw) else {
                return "invalid native details checkpoint".to_owned();
            };
            if details.media.provider_id != provider_id
                || details
                    .episodes
                    .iter()
                    .any(|episode| episode.provider_id != provider_id)
            {
                return "checkpoint provider identity mismatch".to_owned();
            }
            let Ok(native) = serde_json::to_string(&details) else {
                return "invalid native details checkpoint".to_owned();
            };
            if native.len() > MAX_PLUGIN_BODY {
                return "native details checkpoint exceeded its byte limit".to_owned();
            }
            *fallback.lock().unwrap() = Some(native);
            String::new()
        })?,
    )?;
    let allowed = manifest.permissions.addon_metadata;
    let available_host = host.clone();
    let available_caller = format!("nova-provider://{}", manifest.id);
    metadata.set(
        "available",
        Function::new(ctx.clone(), move || {
            allowed
                && !crate::metadata::nested()
                && available_host.metadata_available(&available_caller)
        })?,
    )?;
    let caller = format!("nova-provider://{}", manifest.id);
    let metadata_host = host.clone();
    metadata.set(
        "enrich",
        Function::new(ctx.clone(), move |raw: String| {
            if !allowed {
                return serde_json::json!({"error":"addon metadata permission denied"}).to_string();
            }
            if raw.len() > MAX_PLUGIN_BODY {
                return serde_json::json!({"error":"metadata input exceeded its byte limit"})
                    .to_string();
            }
            let request = match serde_json::from_str::<crate::EnrichmentRequest>(&raw) {
                Ok(request) => request,
                Err(error) => return serde_json::json!({"error":error.to_string()}).to_string(),
            };
            let result = metadata_host.metadata_enrich(request, &caller, Duration::from_secs(5));
            let output = serde_json::to_string(&result).unwrap_or_default();
            if output.len() > MAX_PLUGIN_BODY {
                serde_json::json!({"error":"metadata output exceeded its byte limit"}).to_string()
            } else {
                output
            }
        })?,
    )?;
    metadata.set(
        "resolveEpisode",
        Function::new(ctx.clone(), move |raw: String| {
            if !allowed {
                return serde_json::json!({"error":"addon metadata permission denied"}).to_string();
            }
            if raw.len() > MAX_PLUGIN_BODY {
                return serde_json::json!({"error":"mapping input exceeded its byte limit"})
                    .to_string();
            }
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Request {
                target: crate::StreamLookupRequest,
                source_sequences: Vec<crate::SourceSequence>,
            }
            match serde_json::from_str::<Request>(&raw) {
                Ok(request) => serde_json::to_string(&crate::resolve_episode(
                    &request.target,
                    &request.source_sequences,
                ))
                .unwrap_or_default(),
                Err(error) => serde_json::json!({"error":error.to_string()}).to_string(),
            }
        })?,
    )?;
    nova.set("metadata", metadata)?;
    let host_ref = host;
    nova.set(
        "log",
        Function::new(ctx.clone(), move |message: String| {
            host_ref.log(&message.chars().take(512).collect::<String>());
        })?,
    )?;

    // Pure, bounded crypto helpers keep hoster protocol handling in the
    // provider without giving scripts access to native module loading.
    crypto.set(
        "base64UrlEncode",
        Function::new(ctx.clone(), |value: String| {
            if value.len() > MAX_PLUGIN_BODY {
                String::new()
            } else {
                base64_url_encode(value.as_bytes())
            }
        })?,
    )?;
    crypto.set(
        "base64UrlDecode",
        Function::new(ctx.clone(), |value: String| base64_url_decode(&value))?,
    )?;
    crypto.set(
        "rc4Base64Url",
        Function::new(ctx.clone(), |input: String, key: String| {
            rc4_base64_url(&input, &key)
        })?,
    )?;
    crypto.set(
        "aes256CbcDecrypt",
        Function::new(ctx.clone(), |input: String, key: String, iv: String| {
            aes256_cbc_decrypt(&input, &key, &iv).unwrap_or_default()
        })?,
    )?;
    crypto.set(
        "hmacSha256Base64Url",
        Function::new(ctx.clone(), |input: String, key: String| {
            if input.len() > MAX_PLUGIN_BODY || key.len() > 1024 {
                return String::new();
            }
            let key = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, key.as_bytes());
            base64_url_encode(aws_lc_rs::hmac::sign(&key, input.as_bytes()).as_ref())
        })?,
    )?;

    nova.set("http", http)?;
    nova.set("html", html)?;
    nova.set("crypto", crypto)?;
    nova.set("storage", storage)?;
    ctx.globals().set("nova", nova)?;
    Ok(())
}

fn install_settings<'js>(
    ctx: &rquickjs::Ctx<'js>,
    nova: &Object<'js>,
    manifest: &PluginManifest,
    initial: Option<serde_json::Map<String, serde_json::Value>>,
    generation: u64,
    host: Arc<dyn ProviderHost>,
) -> rquickjs::Result<()> {
    let settings = Object::new(ctx.clone())?;
    let cache = Object::new(ctx.clone())?;
    let writes = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let plugin = manifest.id.clone();
    let schema = manifest.configuration.clone();
    let initial = initial.unwrap_or_else(|| {
        crate::plugin_settings::settings_snapshot(&plugin, &schema)
            .map(|s| s.values)
            .unwrap_or_default()
    });
    let mut validated = serde_json::Map::new();
    for field in &schema {
        let value = initial
            .get(&field.key)
            .filter(|value| field.validate(value).is_ok())
            .unwrap_or(&field.default);
        if field.validate(value).is_ok() {
            validated.insert(field.key.clone(), value.clone());
        }
    }
    let cache_secrets = schema
        .iter()
        .filter(|field| field.kind == "secret")
        .filter_map(|field| {
            validated
                .get(&field.key)
                .and_then(serde_json::Value::as_str)
        })
        .flat_map(|value| [value, value.trim()])
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let snapshot = Arc::new(Mutex::new(validated));
    let read_count = reads.clone();
    let values = snapshot.clone();
    settings.set(
        "get",
        Function::new(ctx.clone(), move |key: String| {
            if key.len() > 128 || read_count.fetch_add(1, Ordering::Relaxed) >= 256 {
                return "null".to_owned();
            }
            values
                .lock()
                .ok()
                .and_then(|values| values.get(&key).cloned())
                .unwrap_or(serde_json::Value::Null)
                .to_string()
        })?,
    )?;
    let id = plugin.clone();
    let fields = schema.clone();
    let values = snapshot.clone();
    let count = writes.clone();
    settings.set(
        "set",
        Function::new(ctx.clone(), move |key: String, raw: String| {
            if raw.len() > 16384 || key.len() > 128 || count.fetch_add(1, Ordering::Relaxed) >= 128
            {
                return "provider settings write limit exceeded".to_owned();
            }
            let Some(field) = fields.iter().find(|field| field.key == key) else {
                return "unknown provider setting".to_owned();
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
                return "invalid provider setting JSON".to_owned();
            };
            if field.validate(&value).is_err() {
                return "invalid provider setting value".to_owned();
            }
            match crate::plugin_settings::set_setting(&id, &key, value.clone()) {
                Ok(()) => {
                    if let Ok(mut values) = values.lock() {
                        values.insert(key, value);
                    }
                    String::new()
                }
                Err(_) => "could not save provider setting".into(),
            }
        })?,
    )?;
    let id = plugin.clone();
    let fields = schema.clone();
    let values = snapshot;
    let count = writes.clone();
    settings.set(
        "remove",
        Function::new(ctx.clone(), move |key: String| {
            if key.len() > 128
                || count.fetch_add(1, Ordering::Relaxed) >= 128
                || !fields.iter().any(|field| field.key == key)
            {
                return "invalid provider settings removal".to_owned();
            }
            match crate::plugin_settings::remove_setting(&id, &key) {
                Ok(()) => {
                    if let Some(field) = fields.iter().find(|field| field.key == key)
                        && let Ok(mut values) = values.lock()
                    {
                        values.insert(key, field.default.clone());
                    }
                    String::new()
                }
                Err(_) => "could not remove provider setting".into(),
            }
        })?,
    )?;
    // These methods share operation counters with durable cache access, but
    // never involve nova.storage or the configured settings backend.
    let id = plugin.clone();
    let memory_host = host.clone();
    let count = reads.clone();
    cache.set(
        "getMemory",
        Function::new(ctx.clone(), move |key: String| {
            if memory_host.cancelled()
                || key.len() > 128
                || count.fetch_add(1, Ordering::Relaxed) >= 256
            {
                return String::new();
            }
            crate::plugin_settings::cache_get_memory_for_generation(
                &id,
                &key,
                unix_seconds(),
                generation,
            )
            .ok()
            .flatten()
            .unwrap_or_default()
        })?,
    )?;
    let id = plugin.clone();
    let memory_host = host.clone();
    let count = writes.clone();
    let secrets = cache_secrets.clone();
    cache.set(
        "setMemory",
        Function::new(ctx.clone(), move |key: String, value: String, ttl: u64| {
            if memory_host.cancelled()
                || key.len() > 128
                || value.len() > 1024 * 1024
                || cache_contains_secret(&key, &secrets)
                || cache_contains_secret(&value, &secrets)
                || count.fetch_add(1, Ordering::Relaxed) >= 128
            {
                return "provider cache write limit exceeded".to_owned();
            }
            crate::plugin_settings::cache_set_memory_for_generation(
                &id,
                &key,
                &value,
                ttl,
                unix_seconds(),
                generation,
            )
            .map(|()| String::new())
            .unwrap_or_else(|_| "could not save provider cache".into())
        })?,
    )?;
    let id = plugin.clone();
    let memory_host = host.clone();
    let count = writes.clone();
    cache.set(
        "removeMemory",
        Function::new(ctx.clone(), move |key: String| {
            if memory_host.cancelled()
                || key.len() > 128
                || count.fetch_add(1, Ordering::Relaxed) >= 128
            {
                return "provider cache write limit exceeded".to_owned();
            }
            crate::plugin_settings::cache_remove_memory_for_generation(&id, &key, generation)
                .map(|()| String::new())
                .unwrap_or_else(|_| "could not remove provider cache".into())
        })?,
    )?;
    let id = plugin.clone();
    let read_host = host.clone();
    cache.set(
        "get",
        Function::new(ctx.clone(), move |key: String| {
            if read_host.cancelled()
                || crate::plugin_settings::settings_generation() != generation
                || key.len() > 128
                || reads.fetch_add(1, Ordering::Relaxed) >= 256
            {
                return String::new();
            }
            crate::plugin_settings::cache_get_for_generation(&id, &key, unix_seconds(), generation)
                .ok()
                .flatten()
                .unwrap_or_default()
        })?,
    )?;
    let id = plugin.clone();
    let count = writes.clone();
    let write_host = host.clone();
    let secrets = cache_secrets;
    cache.set(
        "set",
        Function::new(ctx.clone(), move |key: String, value: String, ttl: u64| {
            if write_host.cancelled()
                || crate::plugin_settings::settings_generation() != generation
                || key.len() > 128
                || value.len() > 256 * 1024
                || cache_contains_secret(&key, &secrets)
                || cache_contains_secret(&value, &secrets)
                || count.fetch_add(1, Ordering::Relaxed) >= 128
            {
                return "provider cache write limit exceeded".to_owned();
            }
            crate::plugin_settings::cache_set_for_generation(
                &id,
                &key,
                &value,
                ttl,
                unix_seconds(),
                generation,
            )
            .map(|()| String::new())
            .unwrap_or_else(|_| "could not save provider cache".into())
        })?,
    )?;
    cache.set(
        "remove",
        Function::new(ctx.clone(), move |key: String| {
            if host.cancelled()
                || crate::plugin_settings::settings_generation() != generation
                || key.len() > 128
                || writes.fetch_add(1, Ordering::Relaxed) >= 128
            {
                return "provider cache write limit exceeded".to_owned();
            }
            crate::plugin_settings::cache_remove_for_generation(&plugin, &key, generation)
                .map(|()| String::new())
                .unwrap_or_else(|_| "could not remove provider cache".into())
        })?,
    )?;
    nova.set("settings", settings)?;
    nova.set("cache", cache)?;
    Ok(())
}

fn cache_contains_secret(raw: &str, secrets: &[String]) -> bool {
    let decoded = percent_encoding::percent_decode_str(raw).decode_utf8_lossy();
    let form = raw.replace('+', " ");
    let form = percent_encoding::percent_decode_str(&form).decode_utf8_lossy();
    secrets
        .iter()
        .any(|secret| raw.contains(secret) || decoded.contains(secret) || form.contains(secret))
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn parse_http_request(raw: &str, domains: &[String]) -> Result<HttpRequest, &'static str> {
    if raw.len() > 300 * 1024 {
        return Err("provider HTTP arguments exceeded their byte limit");
    }
    let request: HttpRequest =
        serde_json::from_str(raw).map_err(|_| "invalid provider HTTP request")?;
    request
        .validate()
        .map_err(|_| "invalid or oversized provider HTTP request")?;
    if !plugin_url_allowed(&request.url, domains) {
        return Err("URL is outside the provider's domain permissions");
    }
    Ok(request)
}

fn http_result(
    result: Result<crate::HttpResponse, crate::ProviderHostError>,
    response_type: HttpResponseType,
) -> serde_json::Value {
    let output = match result {
        Ok(response) if response.body.len() <= MAX_PLUGIN_BODY => {
            let mut output = serde_json::json!({"status":response.status,"url":response.final_url,"contentType":response.content_type});
            match response_type {
                HttpResponseType::Text => {
                    output["body"] = serde_json::Value::String(
                        String::from_utf8_lossy(&response.body).into_owned(),
                    )
                }
                HttpResponseType::Binary => {
                    output["bodyBase64"] = serde_json::Value::String(
                        base64::engine::general_purpose::STANDARD.encode(&response.body),
                    )
                }
            }
            output
        }
        Ok(_) => serde_json::json!({"error":"provider response exceeded the host size limit"}),
        // Host transport errors are deliberately generic and never include URLs,
        // request bodies, account tokens, or upstream error payloads.
        Err(error) => serde_json::json!({"error":crate::host::sanitize_log(&error.to_string())}),
    };
    if output.to_string().len() > MAX_PLUGIN_BODY {
        serde_json::json!({"error":"provider HTTP output exceeded its byte limit"})
    } else {
        output
    }
}

#[derive(Deserialize)]
struct FieldSpec {
    selector: String,
    #[serde(default = "default_text")]
    value: String,
}

fn default_text() -> String {
    "text".to_owned()
}

fn select_html(raw_html: &str, query: &str, fields_json: &str) -> String {
    if raw_html.len() > MAX_PLUGIN_BODY || query.len() > 4096 || fields_json.len() > 16 * 1024 {
        return "[]".to_owned();
    }
    let fields: HashMap<String, FieldSpec> =
        match serde_json::from_str::<HashMap<String, FieldSpec>>(fields_json) {
            Ok(fields) if fields.len() <= 64 => fields,
            Ok(_) => return "[]".to_owned(),
            Err(_) => return "[]".to_owned(),
        };
    let (Ok(query), document) = (Selector::parse(query), Html::parse_document(raw_html)) else {
        return "[]".to_owned();
    };
    let mut rows = Vec::new();
    let mut output_bytes = 0usize;
    for element in document.select(&query).take(4096) {
        let mut row = BTreeMap::new();
        for (name, spec) in &fields {
            let value = if spec.value == "texts" {
                let mut value = String::new();
                if let Ok(selector) = Selector::parse(&spec.selector) {
                    for found in element.select(&selector) {
                        let text = found.text().collect::<String>();
                        let text = text.trim();
                        if text.is_empty() {
                            continue;
                        }
                        if output_bytes + value.len() + text.len() + 2 > MAX_PLUGIN_BODY {
                            return "[]".to_owned();
                        }
                        if !value.is_empty() {
                            value.push_str(", ");
                        }
                        value.push_str(text);
                    }
                }
                value
            } else {
                let found = if spec.selector.is_empty() {
                    Some(element)
                } else {
                    Selector::parse(&spec.selector)
                        .ok()
                        .and_then(|selector| element.select(&selector).next())
                };
                found
                    .map(|found| match spec.value.as_str() {
                        "html" => found.inner_html(),
                        "text" => found.text().collect::<String>().trim().to_owned(),
                        attribute => found.value().attr(attribute).unwrap_or_default().to_owned(),
                    })
                    .unwrap_or_default()
            };
            // Reject expansion while accumulating, before nested selections
            // can retain thousands of copies of the original document.
            output_bytes += name.len() + value.len() + 8;
            if output_bytes > MAX_PLUGIN_BODY {
                return "[]".to_owned();
            }
            row.insert(name.clone(), value);
        }
        rows.push(row);
    }
    serde_json::to_string(&rows)
        .ok()
        .filter(|result| result.len() <= MAX_PLUGIN_BODY)
        .unwrap_or_else(|| "[]".to_owned())
}

#[derive(Default)]
struct XmlNode {
    name: String,
    text: String,
    attributes: BTreeMap<String, String>,
    children: Vec<usize>,
}

fn xml_name_matches(name: &str, query: &str) -> bool {
    name == query || (!query.contains(':') && name.rsplit(':').next() == Some(query))
}

fn select_xml(raw: &str, query: &str, fields_json: &str) -> String {
    select_xml_checked(raw, query, fields_json).unwrap_or_else(|| "[]".into())
}

fn select_xml_checked(raw: &str, query: &str, fields_json: &str) -> Option<String> {
    use quick_xml::{Reader, events::Event};
    if raw.len() > MAX_PLUGIN_BODY
        || query.len() > 128
        || query.is_empty()
        || fields_json.len() > 16384
    {
        return None;
    }
    let fields: BTreeMap<String, FieldSpec> = serde_json::from_str(fields_json).ok()?;
    if fields.len() > 64
        || fields
            .values()
            .any(|field| field.selector.len() > 256 || field.selector.split('/').count() > 16)
    {
        return None;
    }
    let mut reader = Reader::from_str(raw);
    let mut nodes = vec![XmlNode::default()];
    let mut stack = vec![0usize];
    loop {
        let event = reader.read_event().ok()?;
        let empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(tag) | Event::Empty(tag) => {
                if nodes.len() >= 16384 || stack.len() >= 64 {
                    return None;
                }
                let mut node = XmlNode {
                    name: tag.name().0.to_owned(),
                    ..Default::default()
                };
                for attr in tag.attributes() {
                    if node.attributes.len() >= 64 {
                        return None;
                    }
                    let attr = attr.ok()?;
                    node.attributes.insert(
                        attr.key.0.to_owned(),
                        attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            .ok()?
                            .into_owned(),
                    );
                }
                let index = nodes.len();
                nodes[*stack.last()?].children.push(index);
                nodes.push(node);
                if !empty {
                    stack.push(index);
                }
            }
            Event::End(_) => {
                if stack.len() <= 1 {
                    return None;
                }
                stack.pop();
            }
            Event::Text(text) => nodes[*stack.last()?].text.push_str(&text.xml10_content()),
            Event::CData(text) => nodes[*stack.last()?].text.push_str(&text.xml10_content()),
            Event::GeneralRef(reference) => {
                let escaped = format!("&{};", reference.xml10_content());
                let decoded = quick_xml::escape::unescape(&escaped).ok()?;
                nodes[*stack.last()?].text.push_str(&decoded);
            }
            Event::DocType(_) => return None,
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 || nodes[0].children.len() != 1 || !nodes[0].text.trim().is_empty() {
        return None;
    }
    let mut rows = Vec::new();
    let mut output_bytes = 0usize;
    for node in nodes
        .iter()
        .filter(|node| xml_name_matches(&node.name, query))
        .take(4096)
    {
        let mut row = BTreeMap::new();
        for (name, field) in &fields {
            let mut selected = vec![node];
            for part in field.selector.split('/').filter(|part| !part.is_empty()) {
                selected = selected
                    .into_iter()
                    .flat_map(|node| {
                        node.children
                            .iter()
                            .map(|&index| &nodes[index])
                            .filter(|child| xml_name_matches(&child.name, part))
                    })
                    .collect();
            }
            let value = if field.value == "attributes" {
                // Returning bounded attribute dictionaries preserves sibling
                // association (for example Torznab item attributes) without
                // introducing XPath or external entity capabilities.
                serde_json::to_string(
                    &selected
                        .iter()
                        .map(|node| &node.attributes)
                        .collect::<Vec<_>>(),
                )
                .ok()?
            } else {
                selected
                    .first()
                    .map(|node| {
                        if field.value == "text" {
                            node.text.trim().to_owned()
                        } else {
                            node.attributes
                                .get(&field.value)
                                .cloned()
                                .unwrap_or_default()
                        }
                    })
                    .unwrap_or_default()
            };
            output_bytes += name.len() + value.len() + 8;
            if output_bytes > MAX_PLUGIN_BODY {
                return None;
            }
            row.insert(name.clone(), value);
        }
        rows.push(row);
    }
    let result = serde_json::to_string(&rows).ok()?;
    (result.len() <= MAX_PLUGIN_BODY).then_some(result)
}

fn plugin_url_allowed(url: &str, domains: &[String]) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    if url.scheme() != "https" || url.port().is_some_and(|port| port != 443) {
        return false;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    match url.host() {
        Some(url::Host::Ipv4(ip)) if !crate::host::is_public_ip(ip.into()) => return false,
        Some(url::Host::Ipv6(ip)) if !crate::host::is_public_ip(ip.into()) => return false,
        Some(url::Host::Domain(host)) => {
            let host = host.trim_end_matches('.');
            if ["localhost", "local", "internal", "home", "lan"]
                .iter()
                .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
            {
                return false;
            }
        }
        _ => {}
    }
    domains.iter().any(|domain| {
        if domain == "*" {
            return true;
        }
        let domain = domain
            .trim_start_matches("*.")
            .trim_end_matches('.')
            .to_ascii_lowercase();
        !domain.is_empty()
            && (host.eq_ignore_ascii_case(&domain)
                || host
                    .to_ascii_lowercase()
                    .strip_suffix(&format!(".{domain}"))
                    .is_some())
    })
}

fn base64_url_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as usize;
        let b = chunk.get(1).copied().unwrap_or(0) as usize;
        let c = chunk.get(2).copied().unwrap_or(0) as usize;
        out.push(TABLE[a >> 2] as char);
        out.push(TABLE[((a & 3) << 4) | (b >> 4)] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((b & 15) << 2) | (c >> 6)] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[c & 63] as char);
        }
    }
    out
}

fn base64_url_decode(value: &str) -> String {
    decode_base64(value)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

fn decode_base64(value: &str) -> Option<Vec<u8>> {
    if value.len() > MAX_PLUGIN_BODY {
        return None;
    }
    let mut value = value.replace('-', "+").replace('_', "/");
    while !value.len().is_multiple_of(4) {
        value.push('=');
    }
    base64::engine::general_purpose::STANDARD.decode(value).ok()
}

fn aes256_cbc_decrypt(input: &str, key: &str, iv: &str) -> Option<String> {
    use aws_lc_rs::{
        cipher::{AES_256, DecryptionContext, PaddedBlockDecryptingKey, UnboundCipherKey},
        iv::FixedLength,
    };
    // Validate key/IV sizes before decoding or allocating large arguments.
    if key.len() > 44 || iv.len() > 24 {
        return None;
    }
    let key = decode_base64(key)?;
    let iv = decode_base64(iv)?;
    let mut ciphertext = decode_base64(input)?;
    if ciphertext.is_empty() || !ciphertext.len().is_multiple_of(16) {
        return None;
    }
    let key = UnboundCipherKey::new(&AES_256, &key).ok()?;
    let decryptor = PaddedBlockDecryptingKey::cbc_pkcs7(key).ok()?;
    let context = DecryptionContext::Iv128(FixedLength::try_from(iv.as_slice()).ok()?);
    let plaintext = decryptor.decrypt(&mut ciphertext, context).ok()?;
    String::from_utf8(plaintext.to_vec()).ok()
}

fn rc4_base64_url(input: &str, key: &str) -> String {
    if key.is_empty() || key.len() > 1024 || input.len() > MAX_PLUGIN_BODY {
        return String::new();
    }
    let key = key.as_bytes();
    let mut state = [0_u8; 256];
    for (index, byte) in state.iter_mut().enumerate() {
        *byte = index as u8;
    }
    let mut j = 0_usize;
    for i in 0..256 {
        j = (j + state[i] as usize + key[i % key.len()] as usize) & 255;
        state.swap(i, j);
    }
    let mut i = 0_usize;
    j = 0;
    let mut output = Vec::with_capacity(input.len());
    for byte in input.as_bytes() {
        i = (i + 1) & 255;
        j = (j + state[i] as usize) & 255;
        state.swap(i, j);
        let key_byte = state[(state[i] as usize + state[j] as usize) & 255];
        output.push(byte ^ key_byte);
    }
    let mut encoded = base64_url_encode(&output);
    while !encoded.len().is_multiple_of(4) {
        encoded.push('=');
    }
    encoded
}

fn script_error(error: rquickjs::Error) -> ProviderError {
    ProviderError::Script(error.to_string())
}

// Called by the serialized service fixture rather than a separate test: cache
// generations are global, so parallel configure/reset fixtures would race.
#[cfg(test)]
pub(crate) fn exercise_memory_cache_runtime() {
    struct Host;
    impl ProviderHost for Host {
        fn get(
            &self,
            _: &str,
            _: &BTreeMap<String, String>,
        ) -> Result<crate::HttpResponse, crate::ProviderHostError> {
            Err(crate::ProviderHostError("unused".into()))
        }
        fn storage_get(&self, _: &str) -> Option<String> {
            None
        }
        fn storage_set(&self, _: &str, _: &str) -> Result<(), crate::ProviderHostError> {
            panic!("memory cache must not use legacy storage")
        }
        fn log(&self, _: &str) {}
    }
    let manifest: PluginManifest = serde_json::from_value(serde_json::json!({
        "formatVersion":1,"id":"memory-fixture","name":"Fixture","version":"1","entry":"fixture.js",
        "permissions":{},"configuration":[{"key":"key","label":"Key","type":"secret","default":"private-fixture-key"}]
    })).unwrap();
    let script = r#"globalThis.novaProvider={handle(){
        const check=(ok)=>{if(!ok)throw new Error('memory cache assertion failed');};
        check(nova.cache.getMemory('missing')==='');
        check(nova.cache.setMemory('large','x'.repeat(1024*1024),60)==='');
        check(nova.cache.getMemory('large').length===1024*1024);
        check(nova.cache.setMemory('too-large','x'.repeat(1024*1024+1),60)!=='');
        check(nova.cache.setMemory('../bad','x',60)!=='');
        check(nova.cache.setMemory('ttl','x',2592001)!=='');
        check(nova.cache.setMemory('secret','private-fixture-key',60)!=='');
        check(nova.cache.setMemory('encoded','%70rivate-fixture-key',60)!=='');
        check(nova.cache.set('secret','private-fixture-key',60)!=='');
        check(nova.cache.removeMemory('large')==='');
        check(nova.cache.getMemory('large')==='');
        return {ok:true};
    }}"#;
    let runtime = PluginRuntime::new(manifest, script);
    assert_eq!(
        runtime
            .invoke(Arc::new(Host), &serde_json::json!({}))
            .unwrap()["ok"],
        true
    );
    let stale = crate::plugin_settings::settings_generation().wrapping_sub(1);
    let runtime = PluginRuntime::new(
        runtime.manifest.clone(),
        r#"globalThis.novaProvider={handle(){return {
        set:nova.cache.setMemory('stale','x',60),get:nova.cache.getMemory('stale'),remove:nova.cache.removeMemory('stale')
    };}}"#,
    );
    let result = runtime
        .invoke(
            Arc::new(Host),
            &serde_json::json!({"settings":{},"settingsGeneration":stale}),
        )
        .unwrap();
    assert_ne!(result["set"], "");
    assert_eq!(result["get"], "");
    assert_ne!(result["remove"], "");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn script_budget_defaults_and_manifest_bounds() {
        let legacy: PluginLimits = serde_json::from_value(json!({})).unwrap();
        assert_eq!(legacy.script_budget(), Duration::from_secs(5));
        for (requested, expected) in [(10_000, 10_000), (u64::MAX, 10_000), (0, 1)] {
            let limits: PluginLimits =
                serde_json::from_value(json!({"scriptTimeMs": requested})).unwrap();
            assert_eq!(limits.script_budget(), Duration::from_millis(expected));
        }
    }

    #[test]
    fn longer_script_budget_keeps_the_wall_deadline() {
        let mut budget = ExecutionBudget {
            started: Instant::now() - Duration::from_secs(6),
            script_limit: Duration::from_secs(5),
            wall_limit: Duration::from_secs(20),
            http_micros: AtomicU64::new(0),
            host_intervals: Mutex::new(Vec::new()),
        };
        assert!(budget.expired());
        budget.script_limit = Duration::from_secs(10);
        assert!(!budget.expired());
        budget.wall_limit = Duration::from_secs(5);
        budget.http_micros.store(6_000_000, Ordering::Relaxed);
        assert!(budget.expired());
    }

    fn manifest() -> PluginManifest {
        PluginManifest {
            scripts: Vec::new(),
            assets: Vec::new(),
            format_version: 1,
            id: "fixture".into(),
            name: "Fixture".into(),
            version: "1".into(),
            entry: "index.js".into(),
            capabilities: PluginCapabilities::default(),
            permissions: PluginPermissions {
                addon_metadata: false,
                http_post: false,
                ephemeral_playback_credentials: Vec::new(),
                domains: vec!["example.com".into()],
            },
            limits: PluginLimits::default(),
            configuration: Vec::new(),
        }
    }

    #[test]
    fn xml_attribute_arrays_keep_each_items_siblings_associated() {
        let xml = r#"<rss xmlns:torznab="urn:torznab"><channel><item><title>A</title><torznab:attr name="infohash" value="abc"/><torznab:attr name="size" value="42"/></item><item><title>B</title><torznab:attr name="infohash" value="def"/></item></channel></rss>"#;
        let rows:serde_json::Value=serde_json::from_str(&select_xml(xml,"item",r#"{"title":{"selector":"title"},"attributes":{"selector":"torznab:attr","value":"attributes"}}"#)).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(rows[0]["attributes"].as_str().unwrap())
                .unwrap(),
            json!([{"name":"infohash","value":"abc"},{"name":"size","value":"42"}])
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(rows[1]["attributes"].as_str().unwrap())
                .unwrap(),
            json!([{"name":"infohash","value":"def"}])
        );
    }

    #[test]
    fn xml_extraction_decodes_entities_namespaces_and_rejects_dtd() {
        let xml = r#"<rss xmlns:torrent="urn:torrent"><channel><item><title>A &amp; B</title><torrent:infoHash>abcd</torrent:infoHash><enclosure url="https://example.com/a?a=1&amp;b=2"/></item></channel></rss>"#;
        let fields = r#"{"title":{"selector":"title"},"hash":{"selector":"torrent:infoHash"},"url":{"selector":"enclosure","value":"url"}}"#;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&select_xml(xml, "item", fields)).unwrap(),
            json!([{"title":"A & B","hash":"abcd","url":"https://example.com/a?a=1&b=2"}])
        );
        for xml in [
            "<!DOCTYPE rss [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><rss><item>&x;</item></rss>",
            "<rss><item></rss>",
            "<rss><item>&unknown;</item></rss>",
        ] {
            assert_eq!(select_xml(xml, "item", fields), "[]");
        }
        assert_eq!(
            select_xml(
                &format!("{}{}", "<item>".repeat(65), "</item>".repeat(65)),
                "item",
                fields
            ),
            "[]"
        );
    }

    struct BatchFixture {
        barrier: std::sync::Barrier,
        requests: AtomicUsize,
    }

    impl ProviderHost for BatchFixture {
        fn get(
            &self,
            url: &str,
            _headers: &BTreeMap<String, String>,
        ) -> Result<crate::HttpResponse, crate::ProviderHostError> {
            self.requests.fetch_add(1, Ordering::Relaxed);
            self.barrier.wait();
            if url.ends_with("fail") {
                return Err(crate::ProviderHostError("fixture failure".into()));
            }
            Ok(crate::HttpResponse {
                status: 200,
                body: b"ok".to_vec(),
                ..Default::default()
            })
        }
        fn storage_get(&self, _key: &str) -> Option<String> {
            None
        }
        fn storage_set(&self, _key: &str, _value: &str) -> Result<(), crate::ProviderHostError> {
            Ok(())
        }
        fn log(&self, _message: &str) {}
    }

    #[test]
    fn batch_runs_three_independent_gets_and_retains_failures() {
        let host = Arc::new(BatchFixture {
            barrier: std::sync::Barrier::new(3),
            requests: AtomicUsize::new(0),
        });
        let script = r#"globalThis.novaProvider={handle(){return JSON.parse(nova.http.batch(JSON.stringify([{url:'https://example.com/one'},{url:'https://example.com/fail'},{url:'https://example.com/three',responseType:'binary'}])));}}"#;
        let result = PluginRuntime::new(manifest(), script)
            .invoke(host.clone(), &json!({}))
            .unwrap();
        assert_eq!(host.requests.load(Ordering::Relaxed), 3);
        assert_eq!(result[0]["body"], "ok");
        assert!(result[0].get("bodyBase64").is_none());
        assert_eq!(result[1]["error"], "fixture failure");
        assert_eq!(result[2]["bodyBase64"], "b2s=");
        assert!(result[2].get("body").is_none());
    }

    struct CompletionFixture {
        stage: (Mutex<usize>, std::sync::Condvar),
        main_thread: std::thread::ThreadId,
        publications: Mutex<Vec<String>>,
    }
    impl ProviderHost for CompletionFixture {
        fn get(
            &self,
            url: &str,
            _headers: &BTreeMap<String, String>,
        ) -> Result<crate::HttpResponse, crate::ProviderHostError> {
            let needed = if url.ends_with("slow") {
                2
            } else if url.ends_with("fail") {
                1
            } else {
                0
            };
            let (lock, wake) = &self.stage;
            let stage = lock.lock().unwrap();
            let (stage, timeout) = wake
                .wait_timeout_while(stage, Duration::from_secs(2), |stage| *stage < needed)
                .unwrap();
            if timeout.timed_out() && *stage < needed {
                return Err(crate::ProviderHostError(
                    "completion fixture timed out".into(),
                ));
            }
            if url.ends_with("fail") {
                return Err(crate::ProviderHostError("fixture failure".into()));
            }
            Ok(crate::HttpResponse {
                status: 200,
                body: b"ok".to_vec(),
                ..Default::default()
            })
        }
        fn storage_get(&self, _key: &str) -> Option<String> {
            None
        }
        fn storage_set(&self, _key: &str, _value: &str) -> Result<(), crate::ProviderHostError> {
            assert_eq!(std::thread::current().id(), self.main_thread);
            *self.stage.0.lock().unwrap() += 1;
            self.stage.1.notify_all();
            Ok(())
        }
        fn publish_streams(&self, rows: &str) -> Result<(), crate::ProviderHostError> {
            assert_eq!(std::thread::current().id(), self.main_thread);
            self.publications.lock().unwrap().push(rows.to_owned());
            Ok(())
        }
        fn log(&self, _message: &str) {}
    }

    #[test]
    fn batch_each_calls_main_thread_in_completion_order_and_returns_input_order() {
        let host = Arc::new(CompletionFixture {
            stage: (Mutex::new(0), std::sync::Condvar::new()),
            main_thread: std::thread::current().id(),
            publications: Mutex::new(vec![]),
        });
        let script = r#"globalThis.novaProvider={handle(){
          const completed=[];
          const ordered=JSON.parse(nova.http.batchEach(JSON.stringify(['slow','fail','fast'].map(name=>({url:'https://example.com/'+name}))), (index,raw)=>{
            completed.push(index);nova.storage.set('completed',String(index));
            nova.streams.publish(JSON.stringify(completed.map(id=>({id:String(id)}))));
          }));return {completed,ordered};
        }};"#;
        let result = PluginRuntime::new(manifest(), script)
            .invoke(host.clone(), &json!({"op":"lookupStreams"}))
            .unwrap();
        assert_eq!(result["completed"], json!([2, 1, 0]));
        assert_eq!(result["ordered"][0]["body"], "ok");
        assert_eq!(result["ordered"][1]["error"], "fixture failure");
        assert_eq!(result["ordered"][2]["body"], "ok");
        assert_eq!(host.publications.lock().unwrap().len(), 3);
    }

    #[test]
    fn stream_publications_are_operation_and_count_bounded() {
        let host = Arc::new(CompletionFixture {
            stage: (Mutex::new(0), std::sync::Condvar::new()),
            main_thread: std::thread::current().id(),
            publications: Mutex::new(vec![]),
        });
        let script = "globalThis.novaProvider={handle(){return Array.from({length:17},()=>nova.streams.publish('[]'));}}";
        let runtime = PluginRuntime::new(manifest(), script);
        let denied = runtime
            .invoke(host.clone(), &json!({"op":"resolveStream"}))
            .unwrap();
        assert!(denied[0].as_str().unwrap().contains("unavailable"));
        assert!(host.publications.lock().unwrap().is_empty());
        let allowed = runtime
            .invoke(host.clone(), &json!({"op":"lookupStreams"}))
            .unwrap();
        assert_eq!(allowed[15], "");
        assert!(allowed[16].as_str().unwrap().contains("limit"));
        assert_eq!(host.publications.lock().unwrap().len(), 16);
    }

    #[test]
    fn runtime_errors_redact_trimmed_secret_variants() {
        let mut manifest = manifest();
        manifest.configuration=serde_json::from_value(json!([{"key":"key","label":"Key","type":"secret","default":"  whitespace-private-secret\n  "}])).unwrap();
        let script =
            "globalThis.novaProvider={handle(){throw new Error('whitespace-private-secret')}}";
        let error = PluginRuntime::new(manifest, script)
            .invoke(crate::MemoryProviderHost::new(), &json!({}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("[redacted]"));
        assert!(!error.contains("whitespace-private-secret"));
    }

    #[test]
    fn http_response_encoding_does_not_duplicate_large_text_or_binary() {
        let text = serde_json::json!({"success":true,"data":"x".repeat(1_244_000)}).to_string();
        let response = http_result(
            Ok(crate::HttpResponse {
                status: 200,
                body: text.as_bytes().to_vec(),
                ..Default::default()
            }),
            HttpResponseType::Text,
        );
        assert!(response.get("error").is_none());
        assert_eq!(response["body"].as_str(), Some(text.as_str()));
        assert!(response.get("bodyBase64").is_none());
        assert!(response.to_string().len() < MAX_PLUGIN_BODY);
        let bytes = vec![0xff; 512 * 1024];
        let response = http_result(
            Ok(crate::HttpResponse {
                status: 200,
                body: bytes.clone(),
                ..Default::default()
            }),
            HttpResponseType::Binary,
        );
        assert!(response.get("error").is_none());
        assert!(response.get("body").is_none());
        let encoded = response["bodyBase64"].as_str().unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
            bytes
        );
        assert_eq!(encoded.len(), (512usize * 1024).div_ceil(3) * 4);
        let request: HttpRequest =
            serde_json::from_str(r#"{"url":"https://example.com/","responseType":"binary"}"#)
                .unwrap();
        assert_eq!(request.response_type, HttpResponseType::Binary);
        assert_eq!(
            HttpRequest::get("https://example.com/", &BTreeMap::new()).response_type,
            HttpResponseType::Text
        );
    }

    #[test]
    fn post_requires_explicit_permission_and_batch_rejects_mutations() {
        let script = r#"globalThis.novaProvider={handle(){return [JSON.parse(nova.http.request(JSON.stringify({url:'https://example.com/',method:'POST',body:{type:'multipart',value:{magnet:'magnet:test',add_only_if_cached:'true'}}}))),JSON.parse(nova.http.batch(JSON.stringify([{url:'https://example.com/',method:'POST'}])))];}}"#;
        let result = PluginRuntime::new(manifest(), script)
            .invoke(crate::MemoryProviderHost::new(), &json!({}))
            .unwrap();
        assert_eq!(result[0]["error"], "provider POST permission denied");
        assert_eq!(result[1][0]["error"], "HTTP batch supports GET only");
    }

    #[test]
    fn explicit_wildcard_domains_do_not_relax_url_transport_rules() {
        let wildcard = vec!["*".into()];
        assert!(plugin_url_allowed(
            "https://independent.example/",
            &wildcard
        ));
        assert!(plugin_url_allowed("https://1.1.1.1/", &wildcard));
        assert!(!plugin_url_allowed(
            "https://independent.example/",
            &["scoped.example".into()]
        ));
        for url in [
            "http://independent.example/",
            "https://user:secret@independent.example/",
            "https://independent.example:444/",
            "https://127.0.0.1/",
            "https://10.0.0.1/",
            "https://[::1]/",
            "https://localhost/",
            "https://service.internal/",
        ] {
            assert!(!plugin_url_allowed(url, &wildcard));
        }
    }

    #[test]
    fn domain_permissions_reject_credentials_ports_and_suffix_confusion() {
        let domains = vec!["Example.COM".into()];
        assert!(plugin_url_allowed(
            "https://cdn.example.com/video",
            &domains
        ));
        for url in [
            "http://example.com",
            "https://example.com:444",
            "https://example.com.evil.test",
            "https://evilexample.com",
            "https://user:secret@example.com",
            "https://127.0.0.1",
        ] {
            assert!(!plugin_url_allowed(url, &domains), "{url}");
        }
    }

    #[test]
    fn runtime_has_no_native_capabilities_and_storage_is_scoped() {
        let host = crate::MemoryProviderHost::new();
        let script = r#"globalThis.novaProvider = {handle() {
            nova.storage.set("key", "value");
            return [typeof process, typeof require, typeof fetch, typeof std, typeof os,
                nova.storage.get("key"), JSON.parse(nova.http.get("https://evil.test/", "{}")).error];
        }};"#;
        let result = PluginRuntime::new(manifest(), script)
            .invoke(host.clone(), &json!({}))
            .unwrap();
        for index in 0..5 {
            assert_eq!(result[index], "undefined");
        }
        assert_eq!(result[5], "value");
        assert!(result[6].as_str().unwrap().contains("permissions"));
        assert_eq!(host.storage_get("fixture:key").as_deref(), Some("value"));
        assert_eq!(host.storage_get("key"), None);
    }

    #[test]
    fn native_checkpoint_survives_optional_timeout_and_rejects_other_identities() {
        let mut manifest = manifest();
        manifest.limits.request_timeout_ms = 25;
        let details = crate::ProviderDetails {
            media: crate::MediaItem {
                provider_id: manifest.id.clone(),
                source_id: "native".into(),
                media_type: "series".into(),
                title: "Native".into(),
                ..Default::default()
            },
            episodes: vec![],
        };
        let script = "globalThis.novaProvider = {handle(request) {nova.metadata.checkpoint(JSON.stringify(request.details)); while(true) {} }};";
        let request = serde_json::json!({"op":"details","details":details});
        let native = PluginRuntime::new(manifest.clone(), script)
            .invoke(crate::MemoryProviderHost::new(), &request)
            .unwrap();
        assert_eq!(native["media"]["source_id"], "native");
        let mut invalid = request.clone();
        invalid["details"]["media"]["provider_id"] = serde_json::json!("other");
        assert!(
            PluginRuntime::new(manifest.clone(), script)
                .invoke(crate::MemoryProviderHost::new(), &invalid)
                .is_err()
        );
        invalid["op"] = serde_json::json!("streams");
        assert!(
            PluginRuntime::new(manifest, script)
                .invoke(crate::MemoryProviderHost::new(), &invalid)
                .is_err()
        );
    }

    #[test]
    fn runaway_script_and_allocations_are_stopped() {
        let mut limits_manifest = manifest();
        limits_manifest.limits.request_timeout_ms = 25;
        let script = "globalThis.novaProvider = {handle() { while (true) {} }}";
        let started = Instant::now();
        assert!(
            PluginRuntime::new(limits_manifest, script)
                .invoke(crate::MemoryProviderHost::new(), &json!({}))
                .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        let script =
            "globalThis.novaProvider = {handle() { return 'x'.repeat(64 * 1024 * 1024); }}";
        assert!(
            PluginRuntime::new(manifest(), script)
                .invoke(crate::MemoryProviderHost::new(), &json!({}))
                .is_err()
        );
    }

    #[test]
    fn encoding_matches_rc4_vectors_and_preserves_unicode() {
        for value in ["", "abc", "東京 /watch/test", "four"] {
            assert_eq!(
                base64_url_decode(&base64_url_encode(value.as_bytes())),
                value
            );
        }
        // Standard RC4 test vector: key "Key", plaintext "Plaintext".
        assert_eq!(rc4_base64_url("Plaintext", "Key"), "u_MW6NlArwrT");
        assert_eq!(rc4_base64_url("P", "Key"), "uw==");
        assert_eq!(base64_url_decode("dGVzdD8_"), "test??");
        assert_eq!(base64_url_decode("dGVzdD8/"), "test??");
    }

    #[test]
    fn bounded_crypto_helpers_match_independent_vectors() {
        // Ciphertext generated independently with OpenSSL AES-256-CBC.
        let enc = "wdeBruh3qqn_i5wUNnyaPcjCE_9a111S_RppcCnRoC38amAaffSKD9TBNQPLAaJLu1Wubwd8BmyjYHWpom9qr-XFCEplG1DLJvALczHfag1WT_z1sqOWQX1RMYpfz9K3GpZPSOlwXodCezSyKekkeQ";
        let key = "aT9MTVRBeDBRNiw6fTUwVQAAAAAAAAAAAAAAAAAAAAA=";
        let iv = "VzA7MjdUb2FVcGxfUCUnYw==";
        assert_eq!(
            aes256_cbc_decrypt(enc, key, iv).unwrap(),
            format!(
                "{{\"file\":\"https://cdn.example/{}/{}/master.m3u8\"}}",
                "a".repeat(32),
                "b".repeat(32)
            )
        );
        assert!(aes256_cbc_decrypt(enc, "bad", iv).is_none());
        assert!(aes256_cbc_decrypt("bad", key, iv).is_none());
        assert!(aes256_cbc_decrypt(&"a".repeat(MAX_PLUGIN_BODY + 1), key, iv).is_none());
        let script = r#"globalThis.novaProvider = {handle() {
            return [nova.crypto.hmacSha256Base64Url("what do ya want for nothing?", "Jefe"),
                nova.crypto.hmacSha256Base64Url("test", "a".repeat(1025))];
        }};"#;
        let result = PluginRuntime::new(manifest(), script)
            .invoke(crate::MemoryProviderHost::new(), &json!({}))
            .unwrap();
        assert_eq!(result[0], "W9zBRr9gdU5qBCQmCJV1x1oAPwidJzmDnexYuWTsOEM");
        assert_eq!(result[1], "");
    }

    #[test]
    fn css_selection_handles_sibling_episode_titles_and_attributes() {
        let result = select_html(
            "<ul><li><a data-num='1'>1</a><span class='d-title'>First</span></li></ul>",
            "li",
            r#"{"number":{"selector":"a","value":"data-num"},"title":{"selector":".d-title"}}"#,
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&result).unwrap(),
            json!([{"number":"1","title":"First"}])
        );
        let nested = format!(
            "{}{}{}",
            "<div>".repeat(64),
            "x".repeat(40_000),
            "</div>".repeat(64)
        );
        assert_eq!(
            select_html(&nested, "div", r#"{"html":{"selector":"","value":"html"}}"#),
            "[]"
        );
        assert_eq!(
            select_html(
                &nested,
                "html",
                r#"{"texts":{"selector":"div","value":"texts"}}"#
            ),
            "[]"
        );
    }

    #[test]
    fn host_calls_storage_and_final_output_are_bounded() {
        let mut manifest = manifest();
        manifest.limits.requests_per_operation = 1;
        let script = r#"globalThis.novaProvider = {handle() {
            nova.http.get("https://example.com/", "{}");
            return [JSON.parse(nova.http.get("https://example.com/", "{}")).error,
                nova.storage.set("large", "x".repeat(17000))];
        }};"#;
        let result = PluginRuntime::new(manifest, script)
            .invoke(crate::MemoryProviderHost::new(), &json!({}))
            .unwrap();
        assert!(result[0].as_str().unwrap().contains("request limit"));
        assert!(result[1].as_str().unwrap().contains("too large"));
        let script = "globalThis.novaProvider = {handle() { return 'x'.repeat(3 * 1024 * 1024); }}";
        assert!(
            PluginRuntime::new(super::tests::manifest(), script)
                .invoke(crate::MemoryProviderHost::new(), &json!({}))
                .unwrap_err()
                .to_string()
                .contains("output exceeded")
        );
    }
}

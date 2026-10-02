use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use base64::Engine;
use rquickjs::{Context, Function, Object, Runtime};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};

use crate::{ProviderError, ProviderHost};

const MAX_PLUGIN_MEMORY: usize = 32 * 1024 * 1024;
const MAX_PLUGIN_STACK: usize = 512 * 1024;
const MAX_PLUGIN_RUN: Duration = Duration::from_secs(5);
const MAX_PLUGIN_BODY: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub format_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub entry: String,
    pub permissions: PluginPermissions,
    #[serde(default)]
    pub limits: PluginLimits,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PluginPermissions {
    #[serde(default)]
    pub domains: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PluginLimits {
    pub request_timeout_ms: u64,
    pub response_bytes: usize,
    pub redirects: usize,
    pub requests_per_operation: usize,
}

impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            request_timeout_ms: 15_000,
            response_bytes: MAX_PLUGIN_BODY,
            redirects: 5,
            requests_per_operation: 12,
        }
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
        let request = serde_json::to_string(request)
            .map_err(|error| ProviderError::InvalidData(error.to_string()))?;
        if request.len() > MAX_PLUGIN_BODY || self.source.len() > MAX_PLUGIN_BODY {
            return Err(ProviderError::InvalidData(
                "provider input exceeded its byte limit".into(),
            ));
        }
        let budget = Arc::new(ExecutionBudget {
            started: Instant::now(),
            wall_limit: Duration::from_millis(
                self.manifest.limits.request_timeout_ms.clamp(1, 60_000),
            ),
            http_micros: AtomicU64::new(0),
        });
        let host = Arc::new(OperationHost {
            inner: host,
            provider_id: self.manifest.id.clone(),
            budget: budget.clone(),
            request_limit: self.manifest.limits.requests_per_operation.min(64),
            response_bytes: self.manifest.limits.response_bytes.min(MAX_PLUGIN_BODY),
            requests: AtomicUsize::new(0),
            storage_writes: AtomicUsize::new(0),
            log_calls: AtomicUsize::new(0),
        });
        let runtime = Runtime::new().map_err(script_error)?;
        runtime.set_memory_limit(MAX_PLUGIN_MEMORY);
        runtime.set_max_stack_size(MAX_PLUGIN_STACK);
        runtime.set_interrupt_handler(Some(Box::new(move || budget.expired())));
        let context = Context::full(&runtime).map_err(script_error)?;
        let raw = context.with(|ctx| -> Result<String, ProviderError> {
            let convert = |error| {
                ProviderError::Script(rquickjs::CaughtError::from_error(&ctx, error).to_string())
            };
            install_capabilities(&ctx, host.clone(), &self.manifest).map_err(convert)?;
            ctx.eval::<(), _>(self.source.as_str()).map_err(convert)?;
            ctx.globals()
                .set("__nova_request", request)
                .map_err(convert)?;
            ctx.eval::<String, _>(
                "JSON.stringify(globalThis.novaProvider.handle(JSON.parse(__nova_request)))",
            )
            .map_err(convert)
        })?;
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
    provider_id: String,
    budget: Arc<ExecutionBudget>,
    request_limit: usize,
    response_bytes: usize,
    requests: AtomicUsize,
    storage_writes: AtomicUsize,
    log_calls: AtomicUsize,
}

struct ExecutionBudget {
    started: Instant,
    wall_limit: Duration,
    http_micros: AtomicU64,
}

impl ExecutionBudget {
    fn expired(&self) -> bool {
        let elapsed = self.started.elapsed();
        // Blocking host IO consumes the overall operation deadline, but does
        // not spend the script's CPU budget while it waits for the network.
        elapsed >= self.wall_limit
            || elapsed.saturating_sub(Duration::from_micros(
                self.http_micros.load(Ordering::Relaxed),
            )) >= MAX_PLUGIN_RUN
    }
}

impl ProviderHost for OperationHost {
    fn get(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<crate::HttpResponse, crate::ProviderHostError> {
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
        let started = Instant::now();
        let response = self.inner.get(url, headers);
        self.budget.http_micros.fetch_add(
            started.elapsed().as_micros().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        let response = response?;
        if response.body.len() > self.response_bytes {
            return Err(crate::ProviderHostError(
                "provider response exceeded its byte limit".into(),
            ));
        }
        Ok(response)
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
            self.inner.log(&format!("{}: {message}", self.provider_id));
        }
    }
}

fn install_capabilities<'js>(
    ctx: &rquickjs::Ctx<'js>,
    host: Arc<dyn ProviderHost>,
    manifest: &PluginManifest,
) -> rquickjs::Result<()> {
    let nova = Object::new(ctx.clone())?;
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
            match host_ref.get(&url, &headers) {
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
                Err(error) => serde_json::json!({"error":error.to_string()}).to_string(),
            }
        })?,
    )?;

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
    domains.iter().any(|domain| {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest() -> PluginManifest {
        PluginManifest {
            format_version: 1,
            id: "fixture".into(),
            name: "Fixture".into(),
            version: "1".into(),
            entry: "index.js".into(),
            permissions: PluginPermissions {
                domains: vec!["example.com".into()],
            },
            limits: PluginLimits::default(),
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

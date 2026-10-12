use std::{
    collections::BTreeMap,
    fmt,
    io::Read,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use reqwest::{
    Method, Url,
    header::{HeaderName, HeaderValue},
};

const MAX_REQUEST_BODY: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HttpResponseType {
    #[default]
    Text,
    Binary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HttpRequest {
    pub url: String,
    #[serde(default, rename = "timeoutMs")]
    pub timeout_ms: Option<u64>,
    #[serde(default, rename = "responseType")]
    pub response_type: HttpResponseType,
    #[serde(default = "get_method")]
    pub method: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<HttpBody>,
}

fn get_method() -> String {
    "GET".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "lowercase")]
pub enum HttpBody {
    Text(String),
    Json(serde_json::Value),
    Urlencoded(BTreeMap<String, String>),
    Multipart(BTreeMap<String, String>),
}

impl HttpRequest {
    pub fn get(url: &str, headers: &BTreeMap<String, String>) -> Self {
        Self {
            url: url.into(),
            timeout_ms: None,
            response_type: HttpResponseType::Text,
            method: get_method(),
            headers: headers.clone(),
            body: None,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ProviderHostError> {
        if !matches!(self.method.as_str(), "GET" | "POST")
            || (self.method == "GET" && self.body.is_some())
        {
            return Err(ProviderHostError(
                "only GET without a body and POST are supported".into(),
            ));
        }
        if self.url.len() > 8192
            || self.headers.len() > 16
            || self
                .headers
                .iter()
                .map(|(k, v)| k.len() + v.len())
                .sum::<usize>()
                > 8192
        {
            return Err(ProviderHostError(
                "provider HTTP arguments exceeded their byte limit".into(),
            ));
        }
        self.encoded_body()?;
        Ok(())
    }

    fn encoded_body(&self) -> Result<Option<(Vec<u8>, &'static str)>, ProviderHostError> {
        let Some(body) = &self.body else {
            return Ok(None);
        };
        let (bytes, content_type) = match body {
            HttpBody::Text(text) => (text.as_bytes().to_vec(), "text/plain; charset=utf-8"),
            HttpBody::Json(value) => (
                serde_json::to_vec(value)
                    .map_err(|_| ProviderHostError("invalid JSON request body".into()))?,
                "application/json",
            ),
            HttpBody::Urlencoded(fields) => {
                let mut encoded = url::form_urlencoded::Serializer::new(String::new());
                for (key, value) in fields {
                    encoded.append_pair(key, value);
                }
                (
                    encoded.finish().into_bytes(),
                    "application/x-www-form-urlencoded",
                )
            }
            HttpBody::Multipart(fields) => {
                // Text-only forms deliberately exclude filenames and binary uploads.
                // Reject boundary collisions instead of permitting injected parts.
                let boundary = "nova-provider-form-7ad596ab";
                let mut encoded = String::new();
                if fields.len() > 64 {
                    return Err(ProviderHostError("too many form fields".into()));
                }
                for (key, value) in fields {
                    if key.len() > 128
                        || !key
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                        || value.contains(boundary)
                    {
                        return Err(ProviderHostError("invalid multipart field".into()));
                    }
                    encoded.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{key}\"\r\n\r\n{value}\r\n"));
                }
                encoded.push_str(&format!("--{boundary}--\r\n"));
                (
                    encoded.into_bytes(),
                    "multipart/form-data; boundary=nova-provider-form-7ad596ab",
                )
            }
        };
        if bytes.len() > MAX_REQUEST_BODY {
            return Err(ProviderHostError(
                "provider request body exceeded its byte limit".into(),
            ));
        }
        Ok(Some((bytes, content_type)))
    }
}

#[derive(Clone, Debug, Default)]
pub struct HttpResponse {
    pub status: u16,
    pub final_url: String,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ProviderHostError(pub String);

impl fmt::Display for ProviderHostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProviderHostError {}

/// Capabilities exposed to a source implementation. JS code receives this
/// interface through Nova's host API; it never gets a socket or filesystem.
pub trait ProviderHost: Send + Sync {
    fn cancelled(&self) -> bool {
        false
    }

    fn request(&self, request: &HttpRequest) -> Result<HttpResponse, ProviderHostError> {
        request.validate()?;
        if request.method != "GET" {
            return Err(ProviderHostError(
                "HTTP POST is unavailable on this host".into(),
            ));
        }
        self.get(&request.url, &request.headers)
    }

    /// Publish a complete provisional stream list. The registry host validates
    /// ownership and contracts before forwarding to the current app request.
    fn publish_streams(&self, _rows: &str) -> Result<(), ProviderHostError> {
        Ok(())
    }

    fn host_work_elapsed(&self, _duration: Duration) {}

    fn metadata_available(&self, caller: &str) -> bool {
        crate::metadata::has_metadata_addons(caller)
    }

    fn metadata_enrich(
        &self,
        request: crate::EnrichmentRequest,
        caller: &str,
        budget: Duration,
    ) -> crate::EnrichmentResult {
        crate::metadata::enrich_metadata_with_budget(request, caller, budget)
    }
    fn get(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<HttpResponse, ProviderHostError>;

    fn storage_get(&self, key: &str) -> Option<String>;

    fn storage_set(&self, key: &str, value: &str) -> Result<(), ProviderHostError>;

    fn log(&self, message: &str);
}

/// Host-side policy for a provider runtime. Each operation gets a request
/// budget and the HTTP client revalidates and pins DNS results on every
/// redirect before sending the next request.
pub struct ScopedHttpHost {
    base: Arc<dyn ProviderHost>,
    domains: Vec<String>,
    deadline: Instant,
    response_bytes: usize,
    redirect_limit: usize,
    request_limit: usize,
    requests: AtomicUsize,
    total_response_bytes: AtomicUsize,
    total_request_bytes: AtomicUsize,
    cancellation: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl ScopedHttpHost {
    pub fn new(
        base: Arc<dyn ProviderHost>,
        domains: Vec<String>,
        timeout: Duration,
        response_bytes: usize,
        redirect_limit: usize,
        request_limit: usize,
    ) -> Self {
        let timeout = timeout.clamp(Duration::from_millis(1), Duration::from_secs(60));
        Self {
            base,
            domains,
            deadline: Instant::now() + timeout,
            response_bytes: response_bytes.min(2 * 1024 * 1024),
            redirect_limit: redirect_limit.min(10),
            request_limit: request_limit.min(64),
            requests: AtomicUsize::new(0),
            total_response_bytes: AtomicUsize::new(0),
            total_request_bytes: AtomicUsize::new(0),
            cancellation: None,
        }
    }

    pub fn with_cancellation(mut self, cancellation: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    fn check_active(&self) -> Result<(), ProviderHostError> {
        if self.cancelled() {
            return Err(ProviderHostError("provider operation cancelled".into()));
        }
        if Instant::now() >= self.deadline {
            return Err(ProviderHostError(
                "provider exceeded its operation time limit".into(),
            ));
        }
        Ok(())
    }

    fn check_url(
        &self,
        url: &Url,
        deadline: Instant,
    ) -> Result<(String, Vec<SocketAddr>), ProviderHostError> {
        if url.scheme() != "https" || url.port().is_some_and(|port| port != 443) {
            return Err(ProviderHostError(
                "only HTTPS on port 443 is available to providers".into(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(ProviderHostError(
                "provider URLs cannot contain credentials".into(),
            ));
        }
        let host = url
            .host_str()
            .ok_or_else(|| ProviderHostError("provider URL has no host".into()))?
            .to_ascii_lowercase();
        if !self
            .domains
            .iter()
            .any(|allowed| domain_matches(&host, allowed))
        {
            return Err(ProviderHostError(format!(
                "provider has no permission for {host}"
            )));
        }

        let port = url.port_or_known_default().unwrap_or(443);
        let addresses = match url.host() {
            Some(url::Host::Ipv4(address)) => vec![SocketAddr::new(IpAddr::V4(address), port)],
            Some(url::Host::Ipv6(address)) => vec![SocketAddr::new(IpAddr::V6(address), port)],
            _ => {
                let (send, receive) = std::sync::mpsc::sync_channel(1);
                let dns_host = host.clone();
                std::thread::spawn(move || {
                    let result = (dns_host.as_str(), port)
                        .to_socket_addrs()
                        .map(|addresses| addresses.collect::<Vec<_>>());
                    let _ = send.send(result);
                });
                receive
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .map_err(|_| ProviderHostError("provider DNS lookup timed out".into()))?
                    .map_err(|error| {
                        ProviderHostError(format!("DNS lookup failed for {host}: {error}"))
                    })?
            }
        };
        if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(address.ip())) {
            return Err(ProviderHostError(format!(
                "provider host {host} resolved to a non-public address"
            )));
        }
        Ok((host, addresses))
    }
}

impl ProviderHost for ScopedHttpHost {
    fn get(
        &self,
        raw_url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<HttpResponse, ProviderHostError> {
        self.request(&HttpRequest::get(raw_url, headers))
    }

    fn cancelled(&self) -> bool {
        self.base.cancelled() || self.cancellation.as_ref().is_some_and(|check| check())
    }

    fn request(&self, input: &HttpRequest) -> Result<HttpResponse, ProviderHostError> {
        input.validate()?;
        self.check_active()?;
        let deadline = request_deadline(self.deadline, input.timeout_ms);
        let headers = &input.headers;
        let raw_url = &input.url;
        let mut method = if input.method == "POST" {
            Method::POST
        } else {
            Method::GET
        };
        let mut body = input.encoded_body()?;
        let count = self.requests.fetch_add(1, Ordering::Relaxed) + 1;
        if count > self.request_limit {
            return Err(ProviderHostError(
                "provider exceeded its per-operation request limit".into(),
            ));
        }

        let mut target = Url::parse(raw_url)
            .map_err(|error| ProviderHostError(format!("invalid provider URL: {error}")))?;
        let original_origin = target.origin();
        let sensitive = method == Method::POST
            || headers.keys().any(|name| {
                matches!(
                    name.to_ascii_lowercase().as_str(),
                    "authorization" | "cookie"
                )
            });
        for redirect in 0..=self.redirect_limit {
            self.check_active()?;
            if redirect > 0
                && self.requests.fetch_add(1, Ordering::Relaxed) + 1 > self.request_limit
            {
                return Err(ProviderHostError(
                    "provider exceeded its per-operation request limit".into(),
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ProviderHostError("provider HTTP request timed out".into()));
            }
            let (host, addresses) = self.check_url(&target, deadline)?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ProviderHostError("provider HTTP request timed out".into()));
            }
            let builder = reqwest::blocking::Client::builder()
                .no_proxy()
                .user_agent("Mozilla/5.0 (compatible; Nova/0.1)")
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(remaining.min(Duration::from_secs(10)))
                .timeout(remaining)
                .resolve_to_addrs(&host, &addresses);
            #[cfg(target_os = "android")]
            let builder = builder.tls_backend_preconfigured(android_tls_config()?);
            let client = builder.build().map_err(|error| {
                ProviderHostError(format!("could not create provider HTTP client: {error}"))
            })?;
            let mut request = client.request(method.clone(), target.clone());
            if let Some((bytes, content_type)) = &body {
                if self
                    .total_request_bytes
                    .fetch_add(bytes.len(), Ordering::Relaxed)
                    .saturating_add(bytes.len())
                    > 4 * MAX_REQUEST_BODY
                {
                    return Err(ProviderHostError(
                        "provider exceeded its aggregate request body limit".into(),
                    ));
                }
                request = request
                    .header(reqwest::header::CONTENT_TYPE, *content_type)
                    .body(bytes.clone());
            }
            for (name, value) in headers {
                let lower_name = name.to_ascii_lowercase();
                if matches!(
                    lower_name.as_str(),
                    "host" | "content-length" | "connection" | "proxy-authorization"
                ) || (target.origin() != original_origin
                    && matches!(lower_name.as_str(), "authorization" | "cookie"))
                {
                    continue;
                }
                let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
                    continue;
                };
                let Ok(value) = HeaderValue::from_str(value) else {
                    continue;
                };
                request = request.header(name, value);
            }
            self.check_active()?;
            let mut response = request.send().map_err(|error| {
                ProviderHostError(format!(
                    "provider HTTP request failed: {}",
                    if error.is_timeout() {
                        "timeout"
                    } else {
                        "transport error"
                    }
                ))
            })?;
            let status = response.status();
            if status.is_redirection() {
                if redirect == self.redirect_limit {
                    return Err(ProviderHostError(
                        "provider exceeded its redirect limit".into(),
                    ));
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        ProviderHostError("provider redirect has no valid Location".into())
                    })?;
                let next = target
                    .join(location)
                    .map_err(|_| ProviderHostError("invalid provider redirect".into()))?;
                if !redirect_allowed(&target, &next, sensitive) {
                    return Err(ProviderHostError(
                        "authenticated or POST requests cannot redirect across origins".into(),
                    ));
                }
                if matches!(status.as_u16(), 301..=303) && method == Method::POST {
                    method = Method::GET;
                    body = None;
                }
                target = next;
                continue;
            }

            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            if response
                .content_length()
                .is_some_and(|length| length > self.response_bytes as u64)
            {
                return Err(ProviderHostError(
                    "provider response exceeded its byte limit".into(),
                ));
            }
            let mut body = Vec::with_capacity(
                response
                    .content_length()
                    .unwrap_or(0)
                    .min(self.response_bytes as u64) as usize,
            );
            response
                .by_ref()
                .take(self.response_bytes as u64 + 1)
                .read_to_end(&mut body)
                .map_err(|_| ProviderHostError("could not read provider response".into()))?;
            if body.len() > self.response_bytes {
                return Err(ProviderHostError(
                    "provider response exceeded its byte limit".into(),
                ));
            }
            self.check_active()?;
            if Instant::now() >= deadline {
                return Err(ProviderHostError("provider HTTP request timed out".into()));
            }
            if self
                .total_response_bytes
                .fetch_add(body.len(), Ordering::Relaxed)
                .saturating_add(body.len())
                > self.response_bytes.saturating_mul(4)
            {
                return Err(ProviderHostError(
                    "provider exceeded its aggregate response byte limit".into(),
                ));
            }
            return Ok(HttpResponse {
                status: status.as_u16(),
                final_url: target.to_string(),
                content_type,
                body,
            });
        }
        Err(ProviderHostError("provider redirect loop".into()))
    }

    fn storage_get(&self, key: &str) -> Option<String> {
        self.base.storage_get(key)
    }

    fn storage_set(&self, key: &str, value: &str) -> Result<(), ProviderHostError> {
        self.base.storage_set(key, value)
    }

    fn log(&self, message: &str) {
        self.base.log(&sanitize_log(message))
    }
}

fn request_deadline(operation_deadline: Instant, timeout_ms: Option<u64>) -> Instant {
    timeout_ms
        .map(|timeout| {
            (Instant::now() + Duration::from_millis(timeout.clamp(100, 60_000)))
                .min(operation_deadline)
        })
        .unwrap_or(operation_deadline)
}

fn redirect_allowed(current: &Url, next: &Url, sensitive: bool) -> bool {
    !sensitive || current.origin() == next.origin()
}

pub(crate) fn sanitize_log(message: &str) -> String {
    // Diagnostic URLs can contain passkeys or query tokens; omit their full
    // value, as well as common credential fields and authorization headers.
    static SENSITIVE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let regex = SENSITIVE.get_or_init(|| regex::Regex::new(r"(?i)https?://[^\s]+|bearer\s+[^\s]+|(?:api[_-]?key|token|password|authorization)\s*[:=]\s*[^\s,;]+").unwrap());
    let bounded: String = message
        .chars()
        .take(512)
        .filter(|c| !c.is_control())
        .collect();
    regex.replace_all(&bounded, "[redacted]").into_owned()
}

fn domain_matches(host: &str, allowed: &str) -> bool {
    // Only an explicit full wildcard opts out of domain scoping; transport
    // scheme, credentials, DNS pinning and public-IP checks still apply.
    if allowed == "*" {
        return !host.is_empty();
    }
    let allowed = allowed
        .trim_start_matches("*.")
        .trim_end_matches('.')
        .to_ascii_lowercase();
    !allowed.is_empty() && (host == allowed || host.ends_with(&format!(".{allowed}")))
}

pub(crate) fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_broadcast()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && a != 0
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 192 && b == 0 && c == 0)
                && !(a == 192 && b == 0 && c == 2)
                && !(a == 198 && (b == 18 || b == 19))
                && !(a == 198 && b == 51 && c == 100)
                && !(a == 203 && b == 0 && c == 113)
                && a < 224
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(mapped));
            }
            let segments = ip.segments();
            (0x2000..=0x3fff).contains(&segments[0])
                && !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_unique_local()
                && !ip.is_unicast_link_local()
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
                && !(segments[0] == 0x2001 && segments[1] <= 0x01ff)
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && segments[1] <= 0x0fff)
        }
    }
}

#[cfg(target_os = "android")]
fn android_tls_config() -> Result<rustls::ClientConfig, ProviderHostError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| ProviderHostError(format!("TLS versions: {error}")))
    .map(|builder| builder.with_root_certificates(roots).with_no_client_auth())
}

#[derive(Default)]
pub struct MemoryProviderHost {
    values: Mutex<BTreeMap<String, String>>,
}

impl MemoryProviderHost {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

impl ProviderHost for MemoryProviderHost {
    fn get(
        &self,
        _url: &str,
        _headers: &BTreeMap<String, String>,
    ) -> Result<HttpResponse, ProviderHostError> {
        Err(ProviderHostError("no HTTP host was configured".into()))
    }

    fn storage_get(&self, key: &str) -> Option<String> {
        self.values.lock().ok()?.get(key).cloned()
    }

    fn storage_set(&self, key: &str, value: &str) -> Result<(), ProviderHostError> {
        let mut values = self
            .values
            .lock()
            .map_err(|_| ProviderHostError("provider storage is unavailable".into()))?;
        if !values.contains_key(key) && values.len() >= 512 {
            return Err(ProviderHostError(
                "provider storage key limit reached".into(),
            ));
        }
        let total = values
            .iter()
            .filter(|(stored_key, _)| stored_key.as_str() != key)
            .map(|(stored_key, stored_value)| stored_key.len() + stored_value.len())
            .sum::<usize>()
            .saturating_add(key.len())
            .saturating_add(value.len());
        if total > 2 * 1024 * 1024 {
            return Err(ProviderHostError(
                "provider storage size limit reached".into(),
            ));
        }
        values.insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn log(&self, message: &str) {
        eprintln!("nova provider: {}", sanitize_log(message));
    }
}

/// Provider storage and host metadata share one bounded session store.
pub(crate) fn session_state() -> Arc<MemoryProviderHost> {
    static STATE: std::sync::OnceLock<Arc<MemoryProviderHost>> = std::sync::OnceLock::new();
    STATE.get_or_init(MemoryProviderHost::new).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_request_deadline_is_bounded_by_operation_and_minimum_timeout() {
        let started = Instant::now();
        let operation = started + Duration::from_secs(8);
        let request: HttpRequest =
            serde_json::from_str(r#"{"url":"https://example.com","timeoutMs":1500}"#).unwrap();
        let deadline = request_deadline(operation, request.timeout_ms);
        assert!(deadline.duration_since(started) >= Duration::from_millis(1500));
        assert!(deadline.duration_since(started) < Duration::from_secs(2));
        assert_eq!(request_deadline(operation, None), operation);
        assert_eq!(request_deadline(operation, Some(60000)), operation);
        assert!(
            request_deadline(operation, Some(0)).duration_since(started)
                >= Duration::from_millis(100)
        );
    }

    #[test]
    fn post_and_authenticated_redirects_cannot_change_origin() {
        let current = Url::parse("https://example.com/api").unwrap();
        let same = Url::parse("https://example.com/other").unwrap();
        let other = Url::parse("https://cdn.example.com/other").unwrap();
        assert!(redirect_allowed(&current, &same, true));
        assert!(!redirect_allowed(&current, &other, true));
        assert!(redirect_allowed(&current, &other, false));
    }

    #[test]
    fn request_encodings_are_bounded_and_multipart_cannot_inject_parts() {
        let mut request = HttpRequest {
            url: "https://example.com".into(),
            timeout_ms: None,
            response_type: HttpResponseType::Text,
            method: "POST".into(),
            headers: BTreeMap::new(),
            body: Some(HttpBody::Multipart(BTreeMap::from([
                ("magnet".into(), "magnet:test".into()),
                ("add_only_if_cached".into(), "true".into()),
            ]))),
        };
        let (body, content_type) = request.encoded_body().unwrap().unwrap();
        let body = String::from_utf8(body).unwrap();
        assert!(content_type.starts_with("multipart/form-data"));
        assert!(body.contains("name=\"add_only_if_cached\"\r\n\r\ntrue\r\n"));
        request.body = Some(HttpBody::Multipart(BTreeMap::from([(
            "bad\r\nfield".into(),
            "value".into(),
        )])));
        assert!(request.validate().is_err());
        request.body = Some(HttpBody::Text("x".repeat(MAX_REQUEST_BODY + 1)));
        assert!(request.validate().is_err());
        request.body = Some(HttpBody::Urlencoded(BTreeMap::from([(
            "a".into(),
            "x & y".into(),
        )])));
        assert_eq!(request.encoded_body().unwrap().unwrap().0, b"a=x+%26+y");
    }

    #[test]
    fn cancellation_prevents_network_and_diagnostics_hide_credentials() {
        let host = ScopedHttpHost::new(
            MemoryProviderHost::new(),
            vec!["example.com".into()],
            Duration::from_secs(1),
            1024,
            0,
            1,
        )
        .with_cancellation(Arc::new(|| true));
        assert!(
            host.get("https://example.com", &BTreeMap::new())
                .unwrap_err()
                .0
                .contains("cancelled")
        );
        let message = sanitize_log(
            "failed https://api.example.com/?key=secret Bearer topsecret token=hidden",
        );
        assert!(!message.contains("secret"));
        assert!(!message.contains("hidden"));
    }

    #[test]
    fn explicit_wildcard_keeps_transport_and_public_address_policy() {
        assert!(domain_matches("independent.example", "*"));
        assert!(!domain_matches("independent.example", "scoped.example"));
        assert!(!domain_matches("independent.example", "*."));
        let host = ScopedHttpHost::new(
            MemoryProviderHost::new(),
            vec!["*".into()],
            Duration::from_secs(1),
            1024,
            0,
            8,
        );
        for url in [
            "http://independent.example/",
            "https://user:secret@independent.example/",
            "https://independent.example:444/",
            "https://127.0.0.1/",
            "https://10.0.0.1/",
            "https://[::1]/",
        ] {
            assert!(host.get(url, &BTreeMap::new()).is_err());
        }
    }

    #[test]
    fn public_address_policy_blocks_local_and_reserved_networks() {
        for address in [
            "127.0.0.1",
            "10.1.2.3",
            "169.254.169.254",
            "192.168.0.1",
            "100.64.0.1",
            "198.18.0.1",
            "224.1.1.1",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2001::1",
        ] {
            assert!(!is_public_ip(address.parse().unwrap()), "{address}");
        }
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn network_policy_checks_each_target_and_enforces_request_budget() {
        let host = ScopedHttpHost::new(
            MemoryProviderHost::new(),
            vec!["127.0.0.1".into()],
            Duration::from_secs(1),
            1024,
            0,
            2,
        );
        let headers = BTreeMap::new();
        assert!(
            host.get("https://evil.example/", &headers)
                .unwrap_err()
                .to_string()
                .contains("permission")
        );
        assert!(
            host.get("https://127.0.0.1/", &headers)
                .unwrap_err()
                .to_string()
                .contains("non-public")
        );
        assert!(
            host.get("https://127.0.0.1/", &headers)
                .unwrap_err()
                .to_string()
                .contains("request limit")
        );
        assert!(domain_matches("cdn.example.com", "EXAMPLE.COM"));
        assert!(!domain_matches("evil-example.com", "example.com"));
    }
}

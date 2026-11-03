use std::{
    collections::BTreeMap,
    fmt,
    io::Read,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
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

const MAX_DNS_WORKERS: usize = 4;
const MAX_DNS_QUEUED: usize = 16;
const MAX_DNS_ADDRESSES: usize = 16;
const DNS_POLL_INTERVAL: Duration = Duration::from_millis(25);

// A caller may stop waiting for libc DNS, but only the actual resolver worker
// can release its slot. Admission also bounds queued callers; no unbounded job
// queue or detached worker is created when all resolver slots are occupied.
struct DnsPool {
    active: Mutex<usize>,
    changed: Condvar,
    queued: AtomicUsize,
    #[cfg(test)]
    peak_active: AtomicUsize,
    #[cfg(test)]
    peak_queued: AtomicUsize,
}

impl DnsPool {
    fn new() -> Self {
        Self {
            active: Mutex::new(0),
            changed: Condvar::new(),
            queued: AtomicUsize::new(0),
            #[cfg(test)]
            peak_active: AtomicUsize::new(0),
            #[cfg(test)]
            peak_queued: AtomicUsize::new(0),
        }
    }

    fn acquire(
        &'static self,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<DnsPermit, ProviderHostError> {
        // Declare the queue token first so the mutex guard unwinds before it.
        // Its atomic drop also releases a queue position if cancellation panics.
        let mut queued = None;
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        loop {
            check_dns_active(deadline, cancelled)?;
            if *active < MAX_DNS_WORKERS {
                *active += 1;
                #[cfg(test)]
                self.peak_active.fetch_max(*active, Ordering::Relaxed);
                return Ok(DnsPermit(self));
            }
            if queued.is_none() {
                let count = self
                    .queued
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                        (count < MAX_DNS_QUEUED).then_some(count + 1)
                    })
                    .map_err(|_| {
                        ProviderHostError("provider DNS admission queue is full".into())
                    })?
                    + 1;
                queued = Some(DnsQueued(self));
                #[cfg(test)]
                self.peak_queued.fetch_max(count, Ordering::Relaxed);
                #[cfg(not(test))]
                let _ = count;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            active = self
                .changed
                .wait_timeout(active, remaining.min(DNS_POLL_INTERVAL))
                .unwrap_or_else(|error| error.into_inner())
                .0;
        }
    }
}

struct DnsQueued(&'static DnsPool);
impl Drop for DnsQueued {
    fn drop(&mut self) {
        self.0.queued.fetch_sub(1, Ordering::Relaxed);
    }
}

struct DnsPermit(&'static DnsPool);
impl Drop for DnsPermit {
    fn drop(&mut self) {
        let mut active = self
            .0
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *active -= 1;
        self.0.changed.notify_one();
    }
}

fn dns_pool() -> &'static DnsPool {
    static POOL: OnceLock<DnsPool> = OnceLock::new();
    POOL.get_or_init(DnsPool::new)
}

fn check_dns_active(
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), ProviderHostError> {
    if cancelled() {
        return Err(ProviderHostError("provider operation cancelled".into()));
    }
    if Instant::now() >= deadline {
        return Err(ProviderHostError("provider DNS lookup timed out".into()));
    }
    Ok(())
}

type DnsTask = Box<dyn FnOnce() + Send>;

fn launch_dns_worker(task: DnsTask) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("provider-dns".into())
        .spawn(task)
        .map(|_| ())
}

fn resolve_dns_with(
    pool: &'static DnsPool,
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
    resolve: impl FnOnce() -> std::io::Result<Vec<SocketAddr>> + Send + 'static,
    launch: impl FnOnce(DnsTask) -> std::io::Result<()>,
) -> Result<Vec<SocketAddr>, ProviderHostError> {
    let permit = pool.acquire(deadline, cancelled)?;
    check_dns_active(deadline, cancelled)?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    launch(Box::new(move || {
        let _permit = permit;
        let _ = send.send(resolve());
    }))
    .map_err(|_| ProviderHostError("provider DNS worker unavailable".into()))?;
    loop {
        check_dns_active(deadline, cancelled)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receive.recv_timeout(remaining.min(DNS_POLL_INTERVAL)) {
            Ok(result) => {
                // A result racing cancellation/deadline expiry cannot authorize
                // a subsequent connection, even if the OS lookup succeeded.
                check_dns_active(deadline, cancelled)?;
                let addresses =
                    result.map_err(|_| ProviderHostError("provider DNS lookup failed".into()))?;
                validate_dns_addresses(&addresses)?;
                return Ok(addresses);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(ProviderHostError("provider DNS worker unavailable".into()));
            }
        }
    }
}

fn validate_dns_addresses(addresses: &[SocketAddr]) -> Result<(), ProviderHostError> {
    if addresses.is_empty()
        || addresses.len() > MAX_DNS_ADDRESSES
        || addresses.iter().any(|address| !is_public_ip(address.ip()))
    {
        return Err(ProviderHostError(
            "provider DNS returned invalid or non-public addresses".into(),
        ));
    }
    Ok(())
}

// Logical controlled-allocation budget, independent of transport/TLS and RSS.
// A segment is decoded in place. Playlists may overlap input, rewritten output,
// and parser temporaries; charge four body ceilings plus 256 KiB of headroom.
const PLAYBACK_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const PLAYBACK_OVERHEAD_BYTES: usize = 256 * 1024;

struct PlaybackBudget {
    used: Mutex<usize>,
    changed: Condvar,
    limit: usize,
}

impl PlaybackBudget {
    fn acquire(
        &'static self,
        body_limit: usize,
        playlist: bool,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<PlaybackBodyPermit, ProviderHostError> {
        if body_limit > 16 * 1024 * 1024 {
            return Err(ProviderHostError("invalid playback body limit".into()));
        }
        let charge = (body_limit + 1) * if playlist { 4 } else { 1 } + PLAYBACK_OVERHEAD_BYTES;
        if charge > self.limit {
            return Err(ProviderHostError(
                "playback body exceeds memory budget".into(),
            ));
        }
        let mut used = self.used.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            if cancelled() {
                return Err(ProviderHostError("provider operation cancelled".into()));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ProviderHostError(
                    "playback memory admission timed out".into(),
                ));
            }
            if *used <= self.limit - charge {
                *used += charge;
                return Ok(PlaybackBodyPermit {
                    budget: self,
                    charge,
                    body_limit,
                });
            }
            // Cancellation has no notification hook, so bound its observation lag.
            used = self
                .changed
                .wait_timeout(used, remaining.min(Duration::from_millis(25)))
                .unwrap_or_else(|error| error.into_inner())
                .0;
        }
    }
}

pub(crate) struct PlaybackBodyPermit {
    budget: &'static PlaybackBudget,
    charge: usize,
    body_limit: usize,
}

impl PlaybackBodyPermit {
    pub(crate) fn acquire(
        body_limit: usize,
        playlist: bool,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, ProviderHostError> {
        static BUDGET: OnceLock<PlaybackBudget> = OnceLock::new();
        BUDGET
            .get_or_init(|| PlaybackBudget {
                used: Mutex::new(0),
                changed: Condvar::new(),
                limit: PLAYBACK_MEMORY_BYTES,
            })
            .acquire(body_limit, playlist, deadline, cancelled)
    }
}

impl Drop for PlaybackBodyPermit {
    fn drop(&mut self) {
        let mut used = self
            .budget
            .used
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *used -= self.charge;
        self.budget.changed.notify_all();
    }
}

/// Allocate once at the sentinel ceiling: unknown lengths cannot trigger Vec
/// geometric growth. Read at most limit + 1, checking cancellation each chunk.
fn read_bounded_body(
    mut reader: impl Read,
    length: Option<u64>,
    limit: usize,
    active: impl Fn() -> Result<(), ProviderHostError>,
) -> Result<Vec<u8>, ProviderHostError> {
    active()?;
    if length.is_some_and(|length| length > limit as u64) {
        return Err(ProviderHostError(
            "provider response exceeded its byte limit".into(),
        ));
    }
    let mut body = vec![0; limit + 1];
    let mut read = 0;
    loop {
        active()?;
        let end = (read + 64 * 1024).min(body.len());
        let count = match reader.read(&mut body[read..end]) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(ProviderHostError("could not read provider response".into())),
        };
        read += count;
        active()?;
        if read > limit {
            return Err(ProviderHostError(
                "provider response exceeded its byte limit".into(),
            ));
        }
        if count == 0 {
            body.truncate(read);
            return Ok(body);
        }
    }
}

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

#[cfg(test)]
pub(crate) type TestTransport =
    Arc<dyn Fn(&HttpRequest) -> (Box<dyn Read + Send>, Option<u64>) + Send + Sync>;

/// Host-side policy for a provider runtime. Each operation gets a request
/// budget and the HTTP client revalidates and pins DNS results on every
/// redirect before sending the next request.
pub struct ScopedHttpHost {
    base: Arc<dyn ProviderHost>,
    domains: Vec<String>,
    deadline: Instant,
    response_bytes: usize,
    aggregate_response_bytes: usize,
    redirect_limit: usize,
    request_limit: usize,
    requests: AtomicUsize,
    total_response_bytes: AtomicUsize,
    total_request_bytes: AtomicUsize,
    cancellation: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    #[cfg(test)]
    pub(crate) test_transport: Option<TestTransport>,
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
            aggregate_response_bytes: response_bytes.min(2 * 1024 * 1024).saturating_mul(4),
            redirect_limit: redirect_limit.min(10),
            request_limit: request_limit.min(64),
            requests: AtomicUsize::new(0),
            total_response_bytes: AtomicUsize::new(0),
            total_request_bytes: AtomicUsize::new(0),
            cancellation: None,
            #[cfg(test)]
            test_transport: None,
        }
    }

    /// Native relay only. Caller must retain the admitted permit through the
    /// downstream write. Operation response allowance is explicitly four bodies
    /// (playlist 8 MiB, segment 64 MiB, key 256 KiB), distinct from live memory.
    pub(crate) fn playback(
        domains: Vec<String>,
        deadline: Instant,
        permit: &PlaybackBodyPermit,
        cancellation: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        let mut host = Self::new(session_state(), domains, Duration::from_secs(20), 0, 3, 4);
        host.deadline = deadline;
        host.response_bytes = permit.body_limit;
        host.aggregate_response_bytes = permit.body_limit.saturating_mul(4);
        host.cancellation = Some(cancellation);
        host
    }

    fn account_response(&self, bytes: usize) -> Result<(), ProviderHostError> {
        self.total_response_bytes
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |total| {
                total
                    .checked_add(bytes)
                    .filter(|total| *total <= self.aggregate_response_bytes)
            })
            .map(|_| ())
            .map_err(|_| {
                ProviderHostError("provider exceeded its aggregate response byte limit".into())
            })
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

    pub(crate) fn check_url(
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
                let dns_host = host.clone();
                resolve_dns_with(
                    dns_pool(),
                    deadline,
                    &|| self.cancelled(),
                    move || {
                        (dns_host.as_str(), port)
                            .to_socket_addrs()
                            .map(|addresses| addresses.take(MAX_DNS_ADDRESSES + 1).collect())
                    },
                    launch_dns_worker,
                )?
            }
        };
        if addresses.is_empty()
            || addresses.len() > MAX_DNS_ADDRESSES
            || addresses.iter().any(|address| !is_public_ip(address.ip()))
        {
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

        #[cfg(test)]
        if let Some(transport) = &self.test_transport {
            let (reader, length) = transport(input);
            let body = read_bounded_body(reader, length, self.response_bytes, || {
                self.check_active()?;
                if Instant::now() >= deadline {
                    return Err(ProviderHostError("provider HTTP request timed out".into()));
                }
                Ok(())
            })?;
            self.account_response(body.len())?;
            return Ok(HttpResponse {
                status: 200,
                final_url: raw_url.into(),
                content_type: None,
                body,
            });
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
            let length = response.content_length();
            let body = read_bounded_body(&mut response, length, self.response_bytes, || {
                self.check_active()?;
                if Instant::now() >= deadline {
                    return Err(ProviderHostError("provider HTTP request timed out".into()));
                }
                Ok(())
            })?;
            self.check_active()?;
            if Instant::now() >= deadline {
                return Err(ProviderHostError("provider HTTP request timed out".into()));
            }
            self.account_response(body.len())?;
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

pub(crate) fn domain_matches(host: &str, allowed: &str) -> bool {
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
    crate::plugin_settings::session_host()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dns_pool() -> &'static DnsPool {
        Box::leak(Box::new(DnsPool::new()))
    }

    fn wait_dns_empty(pool: &'static DnsPool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut active = pool.active.lock().unwrap();
        while *active != 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "resolver did not return its actual worker permit"
            );
            active = pool.changed.wait_timeout(active, remaining).unwrap().0;
        }
        assert_eq!(pool.queued.load(Ordering::Relaxed), 0);
    }

    fn dns_process_status() -> Option<String> {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .map(|status| {
                status
                    .lines()
                    .filter(|line| {
                        line.starts_with("VmRSS:")
                            || line.starts_with("VmHWM:")
                            || line.starts_with("Threads:")
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
    }

    #[test]
    fn dns_timed_out_callers_keep_actual_workers_bounded_and_recover() {
        let pool = test_dns_pool();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let started = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(100));
        let before = dns_process_status();
        let handles: Vec<_> = (0..100)
            .map(|_| {
                let gate = gate.clone();
                let started = started.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let clock = Instant::now();
                    let result = resolve_dns_with(
                        pool,
                        clock + Duration::from_millis(100),
                        &|| false,
                        move || {
                            started.fetch_add(1, Ordering::Relaxed);
                            let mut released = gate.0.lock().unwrap();
                            while !*released {
                                released = gate.1.wait(released).unwrap();
                            }
                            Ok(vec!["1.1.1.1:443".parse().unwrap()])
                        },
                        launch_dns_worker,
                    );
                    (clock.elapsed().as_micros(), result.err().unwrap().0)
                })
            })
            .collect();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(started.load(Ordering::Relaxed), MAX_DNS_WORKERS);
        assert_eq!(*pool.active.lock().unwrap(), MAX_DNS_WORKERS);
        assert_eq!(pool.queued.load(Ordering::Relaxed), 0);
        assert_eq!(pool.peak_active.load(Ordering::Relaxed), MAX_DNS_WORKERS);
        assert!(pool.peak_queued.load(Ordering::Relaxed) <= MAX_DNS_QUEUED);
        let after_timeouts = dns_process_status();
        // New operations with fresh deadlines still cannot create extra DNS
        // workers while earlier timed-out operations retain their actual slots.
        for _ in 0..10 {
            assert!(
                resolve_dns_with(
                    pool,
                    Instant::now(),
                    &|| false,
                    || panic!("must not resolve"),
                    launch_dns_worker
                )
                .is_err()
            );
        }
        assert_eq!(*pool.active.lock().unwrap(), MAX_DNS_WORKERS);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        wait_dns_empty(pool);
        assert_eq!(
            resolve_dns_with(
                pool,
                Instant::now() + Duration::from_secs(1),
                &|| false,
                || Ok(vec!["[2606:4700:4700::1111]:443".parse().unwrap()]),
                launch_dns_worker
            )
            .unwrap()
            .len(),
            1
        );
        wait_dns_empty(pool);
        eprintln!(
            "MED06 callers=100 active_after_timeouts=4 peak_active={} peak_queued={} final_active=0 before={before:?} after_timeouts={after_timeouts:?} after_recovery={:?} raw_elapsed_us_errors={results:?}",
            pool.peak_active.load(Ordering::Relaxed),
            pool.peak_queued.load(Ordering::Relaxed),
            dns_process_status()
        );
    }

    #[test]
    fn dns_address_limits_spawn_failures_and_worker_panics_release_slots() {
        let pool = test_dns_pool();
        for count in [0, 16, 17] {
            let result = resolve_dns_with(
                pool,
                Instant::now() + Duration::from_secs(1),
                &|| false,
                move || Ok(vec!["1.1.1.1:443".parse().unwrap(); count]),
                launch_dns_worker,
            );
            assert_eq!(result.is_ok(), count == 16);
            wait_dns_empty(pool);
        }
        for addresses in [
            vec![
                "1.1.1.1:443".parse().unwrap(),
                "[2606:4700:4700::1111]:443".parse().unwrap(),
            ],
            vec![
                "1.1.1.1:443".parse().unwrap(),
                "127.0.0.1:443".parse().unwrap(),
            ],
            vec!["[::1]:443".parse().unwrap()],
        ] {
            let public = addresses
                .iter()
                .all(|address: &SocketAddr| is_public_ip(address.ip()));
            assert_eq!(
                resolve_dns_with(
                    pool,
                    Instant::now() + Duration::from_secs(1),
                    &|| false,
                    move || Ok(addresses),
                    launch_dns_worker
                )
                .is_ok(),
                public
            );
            wait_dns_empty(pool);
        }
        assert!(
            resolve_dns_with(
                pool,
                Instant::now() + Duration::from_secs(1),
                &|| false,
                || panic!("failed launch cannot execute resolver"),
                |_| Err(std::io::Error::other("injected spawn failure"))
            )
            .err()
            .unwrap()
            .0
            .contains("unavailable")
        );
        wait_dns_empty(pool);
        assert!(
            resolve_dns_with(
                pool,
                Instant::now() + Duration::from_secs(1),
                &|| false,
                || panic!("injected resolver panic"),
                launch_dns_worker
            )
            .is_err()
        );
        wait_dns_empty(pool);
    }

    #[test]
    fn dns_cancellation_and_deadline_discard_results_and_queue_positions() {
        let pool = test_dns_pool();
        assert!(
            resolve_dns_with(
                pool,
                Instant::now() + Duration::from_secs(1),
                &|| true,
                || panic!("cancelled call must not resolve"),
                launch_dns_worker
            )
            .err()
            .unwrap()
            .0
            .contains("cancelled")
        );
        assert_eq!(*pool.active.lock().unwrap(), 0);
        let permits: Vec<_> = (0..MAX_DNS_WORKERS)
            .map(|_| {
                pool.acquire(Instant::now() + Duration::from_secs(1), &|| false)
                    .unwrap()
            })
            .collect();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal = cancelled.clone();
        let waiter = std::thread::spawn(move || {
            resolve_dns_with(
                pool,
                Instant::now() + Duration::from_secs(2),
                &|| signal.load(Ordering::Relaxed),
                || panic!("queued cancellation must not resolve"),
                launch_dns_worker,
            )
        });
        let until = Instant::now() + Duration::from_secs(1);
        while pool.queued.load(Ordering::Relaxed) == 0 {
            assert!(Instant::now() < until);
            std::thread::yield_now();
        }
        cancelled.store(true, Ordering::Relaxed);
        assert!(
            waiter
                .join()
                .unwrap()
                .err()
                .unwrap()
                .0
                .contains("cancelled")
        );
        assert_eq!(pool.queued.load(Ordering::Relaxed), 0);
        drop(permits);
        wait_dns_empty(pool);
        // Cancellation races a successful resolver completion: no stale answer.
        for deadline_wins in [false, true] {
            let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let signal = cancelled.clone();
            let deadline = Instant::now()
                + if deadline_wins {
                    Duration::from_millis(5)
                } else {
                    Duration::from_secs(1)
                };
            let result = resolve_dns_with(
                pool,
                deadline,
                &|| cancelled.load(Ordering::Relaxed),
                move || {
                    if deadline_wins {
                        std::thread::sleep(Duration::from_millis(15));
                    } else {
                        signal.store(true, Ordering::Relaxed);
                    }
                    Ok(vec!["1.1.1.1:443".parse().unwrap()])
                },
                launch_dns_worker,
            );
            assert!(result.is_err());
            if deadline_wins {
                assert_eq!(*pool.active.lock().unwrap(), 1);
            }
            wait_dns_empty(pool);
        }
    }

    fn budget(limit: usize) -> &'static PlaybackBudget {
        Box::leak(Box::new(PlaybackBudget {
            used: Mutex::new(0),
            changed: Condvar::new(),
            limit,
        }))
    }

    #[test]
    fn playback_body_matrix_and_js_cap() {
        const MIB: usize = 1024 * 1024;
        for size in [
            2 * MIB - 1,
            2 * MIB,
            2 * MIB + 1,
            8 * MIB,
            16 * MIB,
            16 * MIB + 1,
        ] {
            for known in [false, true] {
                let length = known.then_some(size as u64);
                let result = read_bounded_body(
                    std::io::repeat(71).take(size as u64),
                    length,
                    16 * MIB,
                    || Ok(()),
                );
                assert_eq!(
                    result.is_ok(),
                    size <= 16 * MIB,
                    "size={size}, known={known}"
                );
                if let Ok(body) = result {
                    assert_eq!(body.len(), size);
                    assert_eq!(body.capacity(), 16 * MIB + 1);
                }
                let ordinary = ScopedHttpHost::new(
                    MemoryProviderHost::new(),
                    vec![],
                    Duration::from_secs(1),
                    16 * MIB,
                    0,
                    64,
                );
                assert_eq!(ordinary.response_bytes, 2 * MIB);
                assert_eq!(ordinary.aggregate_response_bytes, 8 * MIB);
                assert_eq!(
                    read_bounded_body(
                        std::io::repeat(71).take(size as u64),
                        length,
                        ordinary.response_bytes,
                        || Ok(())
                    )
                    .is_ok(),
                    size <= 2 * MIB
                );
            }
        }
        for (limit, playlist) in [(2 * MIB, true), (16 * MIB, false), (64 * 1024, false)] {
            let permit = budget(PLAYBACK_MEMORY_BYTES)
                .acquire(
                    limit,
                    playlist,
                    Instant::now() + Duration::from_secs(1),
                    &|| false,
                )
                .unwrap();
            let host = ScopedHttpHost::playback(
                vec![],
                Instant::now() + Duration::from_secs(1),
                &permit,
                Arc::new(|| false),
            );
            assert_eq!(host.response_bytes, limit);
            assert_eq!(host.aggregate_response_bytes, 4 * limit);
            for _ in 0..4 {
                host.account_response(limit).unwrap();
            }
            assert!(host.account_response(1).is_err());
            for known in [false, true] {
                for size in [limit, limit + 1] {
                    let consumed = Arc::new(AtomicUsize::new(0));
                    struct Counted {
                        bytes: Arc<AtomicUsize>,
                        remaining: usize,
                    }
                    impl Read for Counted {
                        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                            let count = self.remaining.min(out.len());
                            out[..count].fill(71);
                            self.remaining -= count;
                            self.bytes.fetch_add(count, Ordering::Relaxed);
                            Ok(count)
                        }
                    }
                    let result = read_bounded_body(
                        Counted {
                            bytes: consumed.clone(),
                            remaining: size,
                        },
                        known.then_some(size as u64),
                        limit,
                        || Ok(()),
                    );
                    assert_eq!(result.is_ok(), size <= limit);
                    assert_eq!(
                        consumed.load(Ordering::Relaxed),
                        if known && size > limit { 0 } else { size }
                    );
                }
            }
        }
    }

    #[test]
    fn playback_admission_parallel_readers_and_raii() {
        for budget_bytes in [32 * 1024 * 1024, PLAYBACK_MEMORY_BYTES] {
            for readers in [1, 4, 16] {
                let budget = budget(budget_bytes);
                let peak = Arc::new(AtomicUsize::new(0));
                let barrier = Arc::new(std::sync::Barrier::new(readers));
                let handles: Vec<_> = (0..readers)
                    .map(|_| {
                        let peak = peak.clone();
                        let barrier = barrier.clone();
                        std::thread::spawn(move || {
                            barrier.wait();
                            let permit = budget
                                .acquire(
                                    16 * 1024 * 1024,
                                    false,
                                    Instant::now() + Duration::from_secs(5),
                                    &|| false,
                                )
                                .unwrap();
                            peak.fetch_max(*budget.used.lock().unwrap(), Ordering::Relaxed);
                            std::thread::sleep(Duration::from_millis(5));
                            drop(permit);
                        })
                    })
                    .collect();
                for handle in handles {
                    handle.join().unwrap();
                }
                assert_eq!(*budget.used.lock().unwrap(), 0);
                assert!(peak.load(Ordering::Relaxed) <= budget.limit);
                eprintln!(
                    "MED01 readers={readers} peak_reserved_bytes={} budget={}",
                    peak.load(Ordering::Relaxed),
                    budget.limit
                );
            }
        }
        let budget = budget(PLAYBACK_MEMORY_BYTES);
        let outcome = std::panic::catch_unwind(|| {
            let _permit = budget
                .acquire(
                    16 * 1024 * 1024,
                    false,
                    Instant::now() + Duration::from_secs(1),
                    &|| false,
                )
                .unwrap();
            panic!("injected request panic");
        });
        assert!(outcome.is_err());
        assert_eq!(*budget.used.lock().unwrap(), 0);
    }

    #[test]
    fn playback_wait_and_read_cancel_timeout_and_io_failure_release() {
        let limit = 1024;
        let budget = budget(limit + 1 + PLAYBACK_OVERHEAD_BYTES);
        let held = budget
            .acquire(
                limit,
                false,
                Instant::now() + Duration::from_secs(1),
                &|| false,
            )
            .unwrap();
        assert!(
            budget
                .acquire(
                    limit,
                    false,
                    Instant::now() + Duration::from_millis(5),
                    &|| false
                )
                .err()
                .unwrap()
                .0
                .contains("timed out")
        );
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let trigger = cancelled.clone();
        let waiter = std::thread::spawn(move || {
            budget
                .acquire(
                    limit,
                    false,
                    Instant::now() + Duration::from_secs(1),
                    &|| trigger.load(Ordering::Relaxed),
                )
                .err()
                .unwrap()
                .0
        });
        cancelled.store(true, Ordering::Relaxed);
        assert!(waiter.join().unwrap().contains("cancelled"));
        drop(held);
        for failure in ["cancelled", "timed out", "io"] {
            let _permit = budget
                .acquire(
                    limit,
                    false,
                    Instant::now() + Duration::from_secs(1),
                    &|| false,
                )
                .unwrap();
            let checks = AtomicUsize::new(0);
            struct Broken;
            impl Read for Broken {
                fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                    Err(std::io::Error::other("injected"))
                }
            }
            let reader: Box<dyn Read> = if failure == "io" {
                Box::new(Broken)
            } else {
                Box::new(std::io::repeat(0).take(limit as u64))
            };
            assert!(
                read_bounded_body(reader, None, limit, || {
                    if checks.fetch_add(1, Ordering::Relaxed) >= 2 {
                        Err(ProviderHostError(failure.into()))
                    } else {
                        Ok(())
                    }
                })
                .is_err()
            );
        }
        {
            let _permit = budget
                .acquire(
                    limit,
                    false,
                    Instant::now() + Duration::from_secs(1),
                    &|| false,
                )
                .unwrap();
            struct SlowRead;
            impl Read for SlowRead {
                fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                    std::thread::sleep(Duration::from_millis(5));
                    output[0] = 0;
                    Ok(1)
                }
            }
            let deadline = Instant::now() + Duration::from_millis(1);
            let result = read_bounded_body(SlowRead, None, limit, || {
                if Instant::now() >= deadline {
                    Err(ProviderHostError("timed out".into()))
                } else {
                    Ok(())
                }
            });
            assert!(result.err().unwrap().0.contains("timed out"));
        }
        assert_eq!(*budget.used.lock().unwrap(), 0);
    }

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

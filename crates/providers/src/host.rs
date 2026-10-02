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

use reqwest::{
    Method, Url,
    header::{HeaderName, HeaderValue},
};

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
        }
    }

    fn check_url(&self, url: &Url) -> Result<(String, Vec<SocketAddr>), ProviderHostError> {
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
                    .recv_timeout(self.deadline.saturating_duration_since(Instant::now()))
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
        let count = self.requests.fetch_add(1, Ordering::Relaxed) + 1;
        if count > self.request_limit {
            return Err(ProviderHostError(
                "provider exceeded its per-operation request limit".into(),
            ));
        }

        let mut target = Url::parse(raw_url)
            .map_err(|error| ProviderHostError(format!("invalid provider URL: {error}")))?;
        let original_host = target.host_str().unwrap_or_default().to_ascii_lowercase();
        for redirect in 0..=self.redirect_limit {
            if redirect > 0
                && self.requests.fetch_add(1, Ordering::Relaxed) + 1 > self.request_limit
            {
                return Err(ProviderHostError(
                    "provider exceeded its per-operation request limit".into(),
                ));
            }
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ProviderHostError(
                    "provider exceeded its operation time limit".into(),
                ));
            }
            let (host, addresses) = self.check_url(&target)?;
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ProviderHostError(
                    "provider exceeded its operation time limit".into(),
                ));
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
            let mut request = client.request(Method::GET, target.clone());
            for (name, value) in headers {
                let lower_name = name.to_ascii_lowercase();
                if matches!(
                    lower_name.as_str(),
                    "host" | "content-length" | "connection" | "proxy-authorization"
                ) || (host != original_host
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
            let mut response = request.send().map_err(|error| {
                ProviderHostError(format!("provider HTTP request failed: {error}"))
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
                target = target.join(location).map_err(|error| {
                    ProviderHostError(format!("invalid provider redirect: {error}"))
                })?;
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
                .map_err(|error| {
                    ProviderHostError(format!("could not read provider response: {error}"))
                })?;
            if body.len() > self.response_bytes {
                return Err(ProviderHostError(
                    "provider response exceeded its byte limit".into(),
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
        self.base.log(message)
    }
}

fn domain_matches(host: &str, allowed: &str) -> bool {
    let allowed = allowed
        .trim_start_matches("*.")
        .trim_end_matches('.')
        .to_ascii_lowercase();
    !allowed.is_empty() && (host == allowed || host.ends_with(&format!(".{allowed}")))
}

fn is_public_ip(ip: IpAddr) -> bool {
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
        eprintln!(
            "nova provider: {}",
            message.chars().take(512).collect::<String>()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

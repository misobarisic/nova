//! Platform transport: fire-and-continue HTTP for the app flows.
//!
//! The app logic never blocks and never spawns threads directly. Instead it
//! hands a continuation to [`fetch_bytes`] / [`fetch_image`] and keeps running;
//! the platform layer performs the work and invokes the continuation:
//!
//! * **Native**: an OS thread + blocking `reqwest`, replicating the addon
//!   crate's retry policy (transient 502/503/504 and connect/timeout errors,
//!   two retries with 250/500 ms backoff). Artwork uses the shared recipe pipeline.
//!
//! Continuations always run on worker threads; the callers wrap their UI
//! updates in `slint::invoke_from_event_loop` themselves.

#[cfg(feature = "update-check")]
pub mod updates;

#[path = "public_directory.rs"]
pub mod public_directory;

/// Failure of a fetch, with the HTTP status when the server answered.
#[derive(Debug)]
pub struct FetchError {
    /// HTTP status code, `None` for transport-level failures.
    pub status: Option<u16>,
    pub message: String,
}

impl FetchError {
    fn new(status: Option<u16>, message: impl Into<String>) -> Self {
        FetchError {
            status,
            message: message.into(),
        }
    }

    /// Human-readable message for the UI status line.
    pub fn to_message(&self) -> String {
        self.message.clone()
    }
}

// ---------------------------------------------------------------------------
// Desktop-native implementation (libmpv/reqwest/OpenSSL world)
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "android"))]
mod imp {
    use super::FetchError;
    use addons::Error as AddonError;

    /// Fire a GET on a worker thread; `then` receives the body or error.
    pub fn fetch_bytes(
        url: String,
        then: impl FnOnce(Result<Vec<u8>, FetchError>) + Send + 'static,
    ) {
        fetch_bytes_with_progress(url, std::sync::Arc::new(|_| {}), then);
    }

    /// Full stream snapshots and the final result use this same worker thread.
    /// Ordinary HTTP addons produce only the final continuation.
    pub fn fetch_bytes_with_progress(
        url: String,
        on_progress: std::sync::Arc<dyn Fn(Vec<u8>) + Send + Sync>,
        then: impl FnOnce(Result<Vec<u8>, FetchError>) + Send + 'static,
    ) {
        fetch_bytes_with_progress_for_settings(
            url,
            on_progress,
            nova_providers::plugin_settings::capture_settings_capability(),
            then,
        );
    }

    /// Dispatch with the originating profile's authority, even if another
    /// profile is published before this worker starts.
    pub fn fetch_bytes_with_progress_for_settings(
        url: String,
        on_progress: std::sync::Arc<dyn Fn(Vec<u8>) + Send + Sync>,
        settings: nova_providers::plugin_settings::SettingsCapability,
        then: impl FnOnce(Result<Vec<u8>, FetchError>) + Send + 'static,
    ) {
        fetch_bytes_with_progress_for_owner(
            url,
            on_progress,
            settings,
            nova_providers::OwnedProviderSnapshot::capture(),
            then,
        );
    }

    /// Dispatch and enrich with the exact installed inventory selected by the
    /// originating session, including ordinary HTTP addons and nested lookups.
    pub fn fetch_bytes_with_progress_for_owner(
        url: String,
        on_progress: std::sync::Arc<dyn Fn(Vec<u8>) + Send + Sync>,
        settings: nova_providers::plugin_settings::SettingsCapability,
        inventory: nova_providers::OwnedProviderSnapshot,
        then: impl FnOnce(Result<Vec<u8>, FetchError>) + Send + 'static,
    ) {
        let settings_generation =
            settings.with(nova_providers::plugin_settings::settings_generation);
        let progress_settings = settings.clone();
        let on_progress = std::sync::Arc::new(move |bytes| {
            progress_settings.with(|| {
                if progress_settings.is_current() {
                    on_progress(bytes);
                }
            });
        });
        std::thread::Builder::new()
            .name(format!("fetch {}", short_url(&url)))
            .spawn(move || {
                inventory.with(|| {
                    settings.with(|| {
                        if !settings.is_current()
                            || nova_providers::plugin_settings::settings_generation()
                                != settings_generation
                        {
                            then(Err(FetchError::new(None, "Provider configuration changed")));
                            return;
                        }
                        let result = if let Some(result) =
                            nova_providers::fetch_builtin_addon_with_progress_for_generation(
                                &url,
                                on_progress,
                                settings_generation,
                            ) {
                            result.map_err(|error| FetchError::new(None, error.to_string()))
                        } else {
                            get_blocking(&url).map_err(|error| to_fetch_error(&url, error))
                        };
                        if !settings.is_current() {
                            then(Err(FetchError::new(None, "Provider configuration changed")));
                            return;
                        }
                        let result =
                            result.map(|bytes| nova_providers::enrich_addon_response(&url, bytes));
                        then(if settings.is_current() {
                            result
                        } else {
                            Err(FetchError::new(None, "Provider configuration changed"))
                        });
                    })
                })
            })
            .expect("spawn fetch thread");
    }

    fn get_blocking(url: &str) -> Result<Vec<u8>, AddonError> {
        addons::http_get_blocking(url)
    }

    pub(super) fn blocking_image_bytes(url: &str) -> Option<Vec<u8>> {
        get_blocking(url).ok()
    }

    fn to_fetch_error(url: &str, e: AddonError) -> FetchError {
        match e {
            AddonError::Status { status, .. } => {
                FetchError::new(Some(status), format!("HTTP {status} for {url}"))
            }
            AddonError::InvalidUrl(u) => FetchError::new(None, format!("invalid URL: {u}")),
            AddonError::Json { url: u, source } => {
                FetchError::new(None, format!("invalid JSON from {u}: {source}"))
            }
            #[cfg(not(target_os = "android"))]
            AddonError::Http(err) => FetchError::new(None, err.to_string()),
            #[cfg(target_os = "android")]
            _ => FetchError::new(None, "request failed".to_string()),
        }
    }

    fn short_url(url: &str) -> String {
        url.chars().take(48).collect()
    }
}

// ---------------------------------------------------------------------------
// Android implementation
//
// Same shapes as desktop (worker thread + continuation, recipe-keyed decoded
// cache and processed variants under the app cache dir). Transport
// remains separate: the desktop addons client uses OpenSSL, while Android
// uses rustls. Both decode, resize and compress through nova-media cache.
// ---------------------------------------------------------------------------

#[cfg(target_os = "android")]
mod imp {
    use super::FetchError;
    use std::time::Duration;

    // Recent panics observed anywhere in the process (see
    // `install_panic_hook`), oldest first, bounded. Lets a worker thread
    // report the *inner* panic that killed a helper thread — e.g.
    // reqwest's blocking client only reports "event loop thread panicked"
    // when its background runtime thread dies; the real cause is the
    // earlier entry in this log. A single slot is not enough: the outer
    // re-panic would overwrite the inner cause before it is read.
    static PANIC_LOG: std::sync::LazyLock<std::sync::Mutex<Vec<String>>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));
    const MAX_PANIC_LOG: usize = 5;

    /// Record panic messages process-wide (forwarding to the previous
    /// hook, so logcat still gets them). Call once at startup.
    pub fn install_panic_hook() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current();
            let payload = if let Some(s) = info.payload().downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = info.payload().downcast_ref::<&str>() {
                s.to_string()
            } else {
                String::new()
            };
            let loc = info
                .location()
                .map(|l| l.to_string())
                .unwrap_or_else(|| "?".to_string());
            let msg = if payload.is_empty() {
                format!(
                    "thread '{}' panicked at {loc}",
                    thread.name().unwrap_or("<unnamed>")
                )
            } else {
                format!(
                    "thread '{}' panicked at {loc}: {payload}",
                    thread.name().unwrap_or("<unnamed>")
                )
            };
            if let Ok(mut log) = PANIC_LOG.lock() {
                log.push(msg);
                while log.len() > MAX_PANIC_LOG {
                    log.remove(0);
                }
            }
            prev(info);
        }));
    }

    /// Take the recent panic log (oldest first), clearing it.
    pub(crate) fn take_recent_panics() -> Vec<String> {
        PANIC_LOG
            .lock()
            .ok()
            .map(|mut log| std::mem::take(&mut *log))
            .unwrap_or_default()
    }

    // Built once; a `Result` (not `expect`) so a client-construction
    // failure on-device surfaces as a visible fetch error instead of
    // killing the fetch thread and leaving the UI stuck on "Installing…".
    //
    // TLS is a pinned rustls stack with bundled Mozilla roots, handed to
    // reqwest preconfigured: reqwest's default platform-verifier panics
    // without runtime init ("Expect rustls-platform-verifier to be
    // initialized") and additionally needs a Kotlin AAR that cargo-apk
    // cannot merge into the APK — both failure modes observed on-device.
    static HTTP_CLIENT: std::sync::LazyLock<Result<reqwest::blocking::Client, String>> =
        std::sync::LazyLock::new(|| {
            let tls = tls_config()?;
            reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(15))
                .tls_backend_preconfigured(tls)
                .build()
                .map_err(|e| e.to_string())
        });

    /// rustls client config with bundled Mozilla roots (no platform
    /// verifier, no JNI, no system trust store — pure Rust).
    pub(super) fn tls_config() -> Result<rustls::ClientConfig, String> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("tls versions: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(config)
    }

    /// Extract the message from a caught panic payload (`String`, `&str`,
    /// or anything else) so it can be surfaced in the UI status instead of
    /// a bare "panicked".
    fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
        if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else if let Some(s) = payload.downcast_ref::<&str>() {
            s.to_string()
        } else {
            "unknown panic payload".to_string()
        }
    }

    /// Fire a GET on a worker thread with the addon retry policy
    /// (transient 502/503/504 and connect/timeout errors, two retries with
    /// 250/500 ms backoff); `then` receives the body or error. The fetch is
    /// panic-guarded so `then` always runs — a dying thread with no callback
    /// would leave the UI stuck (e.g. forever on "Installing…").
    pub fn fetch_bytes(
        url: String,
        then: impl FnOnce(Result<Vec<u8>, FetchError>) + Send + 'static,
    ) {
        fetch_bytes_with_progress(url, std::sync::Arc::new(|_| {}), then);
    }

    /// Full stream snapshots and the final result use this same worker thread.
    /// Ordinary HTTP addons produce only the final continuation.
    pub fn fetch_bytes_with_progress(
        url: String,
        on_progress: std::sync::Arc<dyn Fn(Vec<u8>) + Send + Sync>,
        then: impl FnOnce(Result<Vec<u8>, FetchError>) + Send + 'static,
    ) {
        fetch_bytes_with_progress_for_settings(
            url,
            on_progress,
            nova_providers::plugin_settings::capture_settings_capability(),
            then,
        );
    }

    /// Dispatch with the originating profile's authority, even if another
    /// profile is published before this worker starts.
    pub fn fetch_bytes_with_progress_for_settings(
        url: String,
        on_progress: std::sync::Arc<dyn Fn(Vec<u8>) + Send + Sync>,
        settings: nova_providers::plugin_settings::SettingsCapability,
        then: impl FnOnce(Result<Vec<u8>, FetchError>) + Send + 'static,
    ) {
        fetch_bytes_with_progress_for_owner(
            url,
            on_progress,
            settings,
            nova_providers::OwnedProviderSnapshot::capture(),
            then,
        );
    }

    /// Dispatch and enrich with the exact installed inventory selected by the
    /// originating session, including ordinary HTTP addons and nested lookups.
    pub fn fetch_bytes_with_progress_for_owner(
        url: String,
        on_progress: std::sync::Arc<dyn Fn(Vec<u8>) + Send + Sync>,
        settings: nova_providers::plugin_settings::SettingsCapability,
        inventory: nova_providers::OwnedProviderSnapshot,
        then: impl FnOnce(Result<Vec<u8>, FetchError>) + Send + 'static,
    ) {
        let settings_generation =
            settings.with(nova_providers::plugin_settings::settings_generation);
        let progress_settings = settings.clone();
        let on_progress = std::sync::Arc::new(move |bytes| {
            progress_settings.with(|| {
                if progress_settings.is_current() {
                    on_progress(bytes);
                }
            });
        });
        std::thread::Builder::new()
            .name(format!("fetch {}", short_url(&url)))
            .spawn(move || {
                inventory.with(|| {
                    settings.with(|| {
                        if !settings.is_current()
                            || nova_providers::plugin_settings::settings_generation()
                                != settings_generation
                        {
                            then(Err(FetchError::new(None, "Provider configuration changed")));
                            return;
                        }
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let result = if let Some(result) =
                                nova_providers::fetch_builtin_addon_with_progress_for_generation(
                                    &url,
                                    on_progress,
                                    settings_generation,
                                ) {
                                result.map_err(|error| FetchError::new(None, error.to_string()))
                            } else {
                                get_with_retries(&url)
                            };
                            let result = result
                                .map(|bytes| nova_providers::enrich_addon_response(&url, bytes));
                            if settings.is_current() {
                                result
                            } else {
                                Err(FetchError::new(None, "Provider configuration changed"))
                            }
                        }));
                        then(match result {
                            Ok(r) => r,
                            Err(payload) => {
                                let outer = panic_message(payload);
                                let mut msg = format!("fetch thread panicked: {outer}");
                                // The payload above is usually just reqwest's
                                // "event loop thread panicked" (its background
                                // runtime died); the hook log holds the real
                                // cause (earlier entries). Skip entries that
                                // merely repeat the outer message.
                                let causes: Vec<String> = take_recent_panics()
                                    .into_iter()
                                    .filter(|e| !e.contains(&outer))
                                    .collect();
                                if !causes.is_empty() {
                                    msg.push_str(&format!(" [cause: {}]", causes.join(" | ")));
                                }
                                Err(FetchError::new(None, msg))
                            }
                        })
                    })
                })
            })
            .expect("spawn fetch thread");
    }

    pub(super) fn get_with_retries(url: &str) -> Result<Vec<u8>, FetchError> {
        if let Some(result) = nova_providers::fetch_builtin_addon(url) {
            return result.map_err(|error| FetchError::new(None, error.to_string()));
        }
        let mut retries = 0usize;
        loop {
            match get_once(url) {
                Ok(bytes) => return Ok(bytes),
                Err(e) => {
                    let transient = match e.status {
                        Some(status) => addons::TRANSIENT_STATUSES.contains(&status),
                        // Transport-level failure: retryable.
                        None => true,
                    };
                    if transient && retries < addons::MAX_RETRIES {
                        retries += 1;
                        std::thread::sleep(Duration::from_millis(250 << (retries - 1)));
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }

    pub(super) fn blocking_image_bytes(url: &str) -> Option<Vec<u8>> {
        get_with_retries(url).ok()
    }

    fn get_once(url: &str) -> Result<Vec<u8>, FetchError> {
        let client = HTTP_CLIENT
            .as_ref()
            .map_err(|e| FetchError::new(None, format!("http client init failed: {e}")))?;
        let resp = client
            .get(url)
            .send()
            .map_err(|e| FetchError::new(None, format!("request failed: {e}")))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(FetchError::new(
                Some(status),
                format!("HTTP {status} for {url}"),
            ));
        }
        resp.bytes()
            .map(|b| b.to_vec())
            .map_err(|e| FetchError::new(None, format!("read body failed: {e}")))
    }

    fn short_url(url: &str) -> String {
        url.chars().take(48).collect()
    }
}

pub(crate) fn blocking_image_bytes(url: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = nova_providers::provider_asset_bytes(url) {
        return Some(bytes.to_vec());
    }
    imp::blocking_image_bytes(url)
}
#[cfg(target_os = "android")]
pub use imp::install_panic_hook;
pub use imp::{
    fetch_bytes, fetch_bytes_with_progress, fetch_bytes_with_progress_for_owner,
    fetch_bytes_with_progress_for_settings,
};

/// The transport is platform-specific; recipe processing and cache ownership are shared.
pub fn fetch_image(
    url: String,
    image_use: impl Into<crate::cache::ImageUse>,
    then: impl FnOnce(Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>>) + Send + 'static,
) {
    let image_use = image_use.into();
    std::thread::Builder::new()
        .name("image-fetch".into())
        .spawn(move || {
            // A failed worker must still release the caller's loading state.
            let pixels = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::cache::display_pixels(&url, image_use)
            }))
            .ok()
            .flatten();
            then(pixels);
        })
        .expect("spawn image worker");
}

/// Metadata service transport runs on the caller's fetch worker. It never
/// starts a continuation and never accesses app/Slint state.
struct MetadataTransport;
impl nova_providers::AddonMetadataTransport for MetadataTransport {
    fn fetch(
        &self,
        url: &str,
        timeout: std::time::Duration,
        response_limit: usize,
    ) -> Result<Vec<u8>, nova_providers::ProviderHostError> {
        if let Some(result) = nova_providers::fetch_builtin_addon_with_timeout(url, timeout) {
            return result;
        }
        use std::io::Read;
        let builder = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::limited(5));
        #[cfg(target_os = "android")]
        let builder = builder.tls_backend_preconfigured(
            imp::tls_config().map_err(nova_providers::ProviderHostError)?,
        );
        let client = builder
            .build()
            .map_err(|e| nova_providers::ProviderHostError(e.to_string()))?;
        let response = client
            .get(url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|e| nova_providers::ProviderHostError(e.to_string()))?;
        let mut body = Vec::new();
        response
            .take(response_limit as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|e| nova_providers::ProviderHostError(e.to_string()))?;
        if body.len() > response_limit {
            return Err(nova_providers::ProviderHostError(
                "metadata response exceeded its byte limit".into(),
            ));
        }
        Ok(body)
    }
}

pub fn init_metadata_transport() {
    nova_providers::set_metadata_transport(std::sync::Arc::new(MetadataTransport));
}

/// Bounded addon metadata GET for account integrations using the same platform
/// TLS configuration as installed providers. No account credentials are sent.
pub fn fetch_metadata_bytes(
    url: &str,
    timeout: std::time::Duration,
    response_limit: usize,
) -> Result<Vec<u8>, nova_providers::ProviderHostError> {
    use nova_providers::AddonMetadataTransport;
    MetadataTransport.fetch(url, timeout, response_limit)
}

/// Fetch a source caption without sharing mpv's global HTTP authority. All
/// video headers are origin-bound, including custom token headers: once a
/// redirect leaves that origin they stay absent for the rest of the chain.
pub fn fetch_source_subtitle_bytes(
    url: &str,
    video_url: &str,
    video_headers: &[(String, String)],
    timeout: std::time::Duration,
    response_limit: usize,
) -> Result<Vec<u8>, String> {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue, LOCATION};
    use std::io::Read;
    let parse = |value: &str| {
        reqwest::Url::parse(value)
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
    };
    let mut url = parse(url).ok_or("invalid source subtitle URL")?;
    let video_origin = parse(video_url).map(|url| url.origin());
    let mut authorized = video_origin.as_ref() == Some(&url.origin());
    let mut headers = HeaderMap::new();
    let mut bytes = 0usize;
    for (name, value) in video_headers.iter().take(16) {
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        if matches!(
            name.as_str(),
            "host" | "content-length" | "connection" | "proxy-authorization"
        ) || value.bytes().any(|byte| byte < b' ' || byte == 127)
        {
            continue;
        }
        let Ok(value) = HeaderValue::from_str(value) else {
            continue;
        };
        bytes = bytes.saturating_add(name.as_str().len() + value.as_bytes().len() + 2);
        if bytes > 8 * 1024 {
            break;
        }
        headers.append(name, value);
    }
    let builder = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none());
    #[cfg(target_os = "android")]
    let builder = builder.tls_backend_preconfigured(imp::tls_config()?);
    let client = builder.build().map_err(|_| "subtitle HTTP client failed")?;
    let deadline = std::time::Instant::now() + timeout;
    for redirects in 0..=5 {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or("subtitle request timed out")?;
        let mut request = client.get(url.clone()).timeout(remaining);
        if authorized {
            request = request.headers(headers.clone());
        }
        let response = request.send().map_err(|_| "subtitle request failed")?;
        if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            if redirects == 5 {
                return Err("too many subtitle redirects".into());
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or("invalid subtitle redirect")?;
            let next = url
                .join(location)
                .ok()
                .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
                .ok_or("invalid subtitle redirect")?;
            authorized &= next.origin() == url.origin();
            url = next;
            continue;
        }
        let response = response
            .error_for_status()
            .map_err(|_| "subtitle HTTP error")?;
        let mut body = Vec::new();
        response
            .take(response_limit as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|_| "subtitle response failed")?;
        if body.len() > response_limit {
            return Err("subtitle response exceeded its byte limit".into());
        }
        return Ok(body);
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_providers::ProviderHostError;
    use nova_providers::plugin_settings::{self as settings, PluginSettingsBackend};
    use std::io::{Cursor, Write};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    #[test]
    fn source_caption_headers_follow_only_same_origin_redirects() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let foreign = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin_url = format!("http://{}", origin.local_addr().unwrap());
        let foreign_url = format!("http://{}", foreign.local_addr().unwrap());
        let observations = Arc::new(Mutex::new(Vec::new()));
        let mut workers = Vec::new();
        for (listener, count) in [(origin, 6), (foreign, 2)] {
            let observations = observations.clone();
            let origin_url = origin_url.clone();
            let foreign_url = foreign_url.clone();
            workers.push(std::thread::spawn(move || {
                listener.set_nonblocking(true).unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                for _ in 0..count {
                    let (mut stream, _) = loop {
                        match listener.accept() {
                            Ok(connection) => break connection,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(std::time::Instant::now() < deadline, "caption request missing");
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            Err(error) => panic!("{error}"),
                        }
                    };
                    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        stream.read_exact(&mut byte).unwrap();
                        request.push(byte[0]);
                    }
                    let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                    let path = request.split_whitespace().nth(1).unwrap().to_owned();
                    let authorized = ["authorization: bearer test", "cookie: caption=test", "x-private-key: custom"]
                        .map(|header| request.contains(header));
                    observations.lock().unwrap().push((path.clone(), authorized));
                    let location = match path.as_str() {
                        "/same" => Some(format!("{origin_url}/private")),
                        "/out" => Some(format!("{foreign_url}/bounce")),
                        "/bounce" => Some(format!("{origin_url}/final")),
                        _ => None,
                    };
                    let response = if let Some(location) = location {
                        format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    } else if path == "/private" && authorized != [true; 3] {
                        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
                    } else {
                        "HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nWEBVTT\n".into()
                    };
                    let _ = stream.write_all(response.as_bytes());
                }
            }));
        }
        let headers = vec![
            ("Authorization".into(), "Bearer test".into()),
            ("Cookie".into(), "caption=test".into()),
            ("X-Private-Key".into(), "custom".into()),
        ];
        for url in [
            format!("{origin_url}/private"),
            format!("{origin_url}/same"),
            format!("{foreign_url}/caption"),
            format!("{origin_url}/out"),
        ] {
            assert_eq!(
                fetch_source_subtitle_bytes(
                    &url,
                    &format!("{origin_url}/video"),
                    &headers,
                    Duration::from_secs(5),
                    1024
                )
                .unwrap(),
                b"WEBVTT\n"
            );
        }
        assert!(
            fetch_source_subtitle_bytes(
                &format!("{origin_url}/private"),
                &format!("{origin_url}/video"),
                &headers,
                Duration::from_secs(5),
                3
            )
            .is_err()
        );
        for worker in workers {
            worker.join().unwrap();
        }
        let observed = observations.lock().unwrap();
        assert_eq!(observed.len(), 8);
        for (path, headers) in observed.iter() {
            assert_eq!(
                *headers,
                if matches!(path.as_str(), "/private" | "/same" | "/out") {
                    [true; 3]
                } else {
                    [false; 3]
                },
                "{path}"
            );
        }
    }

    struct Backend {
        owner: &'static str,
        settings: Mutex<String>,
    }
    impl PluginSettingsBackend for Backend {
        fn owner_id(&self) -> Option<String> {
            Some(self.owner.into())
        }
        fn load_settings(&self) -> Result<Option<String>, ProviderHostError> {
            Ok(Some(self.settings.lock().unwrap().clone()))
        }
        fn save_settings(&self, raw: &str) -> Result<(), ProviderHostError> {
            *self.settings.lock().unwrap() = raw.into();
            Ok(())
        }
        fn load_cache(&self) -> Result<Option<String>, ProviderHostError> {
            Ok(None)
        }
        fn save_cache(&self, _: &str) -> Result<(), ProviderHostError> {
            Ok(())
        }
    }
    fn route() -> serde_json::Value {
        let field = serde_json::from_value(serde_json::json!({
            "key":"route","label":"Route","type":"string","default":""
        }))
        .unwrap();
        settings::settings_snapshot("bytefixture", &[field])
            .unwrap()
            .values["route"]
            .clone()
    }

    fn package(prefix: &str) -> Arc<nova_providers::AddonPackage> {
        let manifest = serde_json::json!({
            "formatVersion":1,"id":"bytefixture","name":prefix,"version":"1.0.0","entry":"index.js","permissions":{},
            "capabilities":{"configurableCapabilities":true},
            "configuration":[{"key":"route","label":"Route","type":"string","default":""}]
        }).to_string();
        let addon = serde_json::json!({"id":"nova.bytefixture","name":prefix,"version":"1.0.0"})
            .to_string();
        let source = "globalThis.novaProvider={handle(request){if(request.op==='manifest')return request.manifest;let rows=[{id:'one',title:'Fixture stream',url:'https://example.test/CODE-'+JSON.parse(nova.settings.get('route'))}];let error=nova.streams.publish(JSON.stringify(rows));if(error!=='')throw new Error(error);return rows;}};".replace("CODE", prefix);
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, contents) in [
            ("manifest.json", manifest.as_str()),
            ("stremio-manifest.json", addon.as_str()),
            ("LICENSE", "Test fixture"),
            ("NOTICE.md", "Test fixture"),
            ("index.js", source.as_str()),
        ] {
            archive
                .start_file(
                    name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            archive.write_all(contents.as_bytes()).unwrap();
        }
        nova_providers::AddonPackage::parse(&archive.finish().unwrap().into_inner()).unwrap()
    }

    #[test]
    fn explicit_byte_fetch_uses_origin_settings_after_switch_and_rejects_revocation() {
        let backend = |owner, value| {
            Arc::new(Backend {
                owner,
                settings: Mutex::new(format!(r#"{{"bytefixture":{{"route":"{value}"}}}}"#)),
            })
        };
        settings::configure_plugin_settings(backend("byte-fetch-alice", "alice")).unwrap();
        let alice = settings::capture_settings_capability();
        let previous_packages = nova_providers::installed_packages();
        let package_a = package("A");
        let config = nova_providers::ProviderCapabilities::default();
        let inventory = nova_providers::OwnedProviderSnapshot::new(
            vec![package_a.clone()],
            std::collections::HashMap::from([(package_a.url(), config)]),
            vec![nova_providers::MetadataAddon {
                url: package_a.url(),
                manifest: package_a.addon_manifest().clone(),
            }],
        );
        let metadata_revision = inventory.with(nova_providers::metadata_revision);
        let package_b = package("B");
        nova_providers::set_installed_packages(vec![package_b.clone()]);
        nova_providers::configure_provider_capabilities(
            &package_b.url(),
            nova_providers::ProviderCapabilities {
                streams: false,
                metadata: false,
            },
        );
        nova_providers::configure_metadata_addons(vec![nova_providers::MetadataAddon {
            url: "nova-provider://bytefixture/b-only".into(),
            manifest: package_b.addon_manifest().clone(),
        }]);
        settings::configure_plugin_settings(backend("byte-fetch-bob", "bob")).unwrap();
        assert_ne!(nova_providers::metadata_revision(), metadata_revision);
        let (progress_tx, progress_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel();
        fetch_bytes_with_progress_for_owner(
            "nova-provider://bytefixture/stream/movie/title.json".into(),
            Arc::new(move |_| {
                // The same transport used by nested metadata requests must
                // select A's manifest even while B owns the process registry.
                let manifest = fetch_metadata_bytes(
                    "nova-provider://bytefixture/manifest.json",
                    Duration::from_secs(1),
                    65536,
                )
                .unwrap();
                let manifest: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
                progress_tx
                    .send((
                        route(),
                        nova_providers::metadata_revision(),
                        manifest["name"].clone(),
                    ))
                    .unwrap();
            }),
            alice.clone(),
            inventory.clone(),
            move |result| {
                tx.send((result, route(), nova_providers::metadata_revision()))
                    .unwrap()
            },
        );
        let (result, callback_route, callback_revision) =
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&result.unwrap()).unwrap();
        assert_eq!(body["streams"][0]["url"], "https://example.test/A-alice");
        assert_eq!(callback_route, "alice");
        assert_eq!(callback_revision, metadata_revision);
        let (progress_route, progress_revision, nested_manifest) =
            progress_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(progress_route, "alice");
        assert_eq!(progress_revision, metadata_revision);
        assert_eq!(nested_manifest, "A");
        assert_eq!(route(), "bob");

        let (tx, rx) = mpsc::channel();
        fetch_bytes_with_progress_for_owner(
            "nova-provider://bytefixture/manifest.json".into(),
            Arc::new(|_| {}),
            alice.clone(),
            inventory.clone(),
            move |result| tx.send(result).unwrap(),
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap())
                .unwrap();
        assert_eq!(manifest["name"], "A");
        let (tx, rx) = mpsc::channel();
        fetch_bytes_with_progress_for_owner(
            "nova-provider://bytefixture/manifest.json".into(),
            Arc::new(|_| {}),
            alice.clone(),
            nova_providers::OwnedProviderSnapshot::new(vec![], Default::default(), vec![]),
            move |result| tx.send(result).unwrap(),
        );
        assert!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap().is_err(),
            "missing owner package cannot fall back to B"
        );

        alice.with(|| settings::set_setting("bytefixture", "route", "changed".into()).unwrap());
        let (tx, rx) = mpsc::channel();
        fetch_bytes_with_progress_for_owner(
            "nova-provider://bytefixture/stream/movie/title.json".into(),
            Arc::new(|_| panic!("revoked owner must not publish progress")),
            alice,
            inventory,
            move |result| tx.send(result).unwrap(),
        );
        assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap().is_err());
        nova_providers::set_installed_packages(previous_packages);
    }
}

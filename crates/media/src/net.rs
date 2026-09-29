//! Platform transport: fire-and-continue HTTP for the app flows.
//!
//! The app logic never blocks and never spawns threads directly. Instead it
//! hands a continuation to [`fetch_bytes`] / [`fetch_image`] and keeps running;
//! the platform layer performs the work and invokes the continuation:
//!
//! * **Native**: an OS thread + blocking `reqwest`, replicating the addon
//!   crate's retry policy (transient 502/503/504 and connect/timeout errors,
//!   two retries with 250/500 ms backoff). `fetch_image` reuses the existing
//!   `poster_pixels` pipeline (decoded LRU + disk cache + `image` decoding).
//!
//! Continuations always run on worker threads; the callers wrap their UI
//! updates in `slint::invoke_from_event_loop` themselves.

/// Failure of a fetch, with the HTTP status when the server answered.
#[derive(Debug)]
pub struct FetchError {
    /// HTTP status code, `None` for transport-level failures.
    pub status: Option<u16>,
    pub message: String,
}

impl FetchError {
    fn new(status: Option<u16>, message: impl Into<String>) -> Self {
        FetchError { status, message: message.into() }
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
        std::thread::Builder::new()
            .name(format!("fetch {}", short_url(&url)))
            .spawn(move || then(get_blocking(&url).map_err(|e| to_fetch_error(&url, e))))
            .expect("spawn fetch thread");
    }

    /// Fire a GET + decode on a worker thread; `then` receives the decoded
    /// RGBA pixels (reusing the poster LRU + disk cache pipeline).
    /// `max_side` caps the longest side for display use (sized LRU key +
    /// small-file tier, see `app::display_pixels`); `None` = full fidelity.
    pub fn fetch_image(
        url: String,
        max_side: Option<u32>,
        then: impl FnOnce(Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>>) + Send + 'static,
    ) {
        std::thread::Builder::new()
            .name(format!("image {}", short_url(&url)))
            .spawn(move || {
                then(crate::cache::display_pixels(&url, max_side));
            })
            .expect("spawn image thread");
    }

    /// Fire a GET + decode on a worker thread, bypassing every cache tier
    /// (memory LRU and disk): `then` receives freshly downloaded pixels
    /// (plus the raw bytes) or `None`. Used for refresh checks that must
    /// detect same-URL art changes instead of serving cached content.
    /// Nothing is cached here — the caller compares and stores on change.
    pub(crate) fn fetch_image_fresh(
        url: String,
        max_side: Option<u32>,
        then: impl FnOnce(Option<crate::cache::FreshImage>) + Send + 'static,
    ) {
        std::thread::Builder::new()
            .name(format!("image-refresh {}", short_url(&url)))
            .spawn(move || {
                then(crate::cache::download_image_fresh(&url, max_side));
            })
            .expect("spawn image-refresh thread");
    }

    fn get_blocking(url: &str) -> Result<Vec<u8>, AddonError> {
        addons::http_get_blocking(url)
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
// Same shapes as desktop (worker thread + continuation, sized-LRU image
// cache plus the on-disk raw-byte tier under the app cache dir) but
// self-contained: the `addons` blocking client and the desktop poster
// pipeline are unavailable here (no `client` feature, no re-encode
// machinery), and TLS runs through rustls (OpenSSL does not cross-compile
// to Android). Decoding uses the pure-Rust `image` crate.
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
                format!("thread '{}' panicked at {loc}", thread.name().unwrap_or("<unnamed>"))
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
        PANIC_LOG.lock().ok().map(|mut log| std::mem::take(&mut *log)).unwrap_or_default()
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
    fn tls_config() -> Result<rustls::ClientConfig, String> {
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
        std::thread::Builder::new()
            .name(format!("fetch {}", short_url(&url)))
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    get_with_retries(&url)
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
            .expect("spawn fetch thread");
    }

    /// Fire a GET + decode on a worker thread; `then` receives RGBA pixels
    /// (session sized-LRU plus the on-disk raw-byte tier under the app
    /// cache dir — same `posters/<hash>.img` layout as desktop, so a
    /// restart never refetches). Honors the `cache_images` setting like
    /// desktop's `original_bytes`: with caching off, every load downloads.
    pub fn fetch_image(
        url: String,
        max_side: Option<u32>,
        then: impl FnOnce(Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>>) + Send + 'static,
    ) {
        std::thread::Builder::new()
            .name(format!("image {}", short_url(&url)))
            .spawn(move || {
                let pixels = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let key = crate::cache::sized_cache_key(&url, max_side);
                    if let Some(pixels) = crate::cache::decoded_cache_get(&key) {
                        return Some(pixels);
                    }
                    let cache_images = nova_config::active_cache_settings().cache_images;
                    let dir = nova_config::poster_cache_dir();
                    if cache_images
                        && let Some(bytes) = crate::cache::read_poster_bytes(&dir, &url)
                        && let Some(pixels) = decode_and_fit(&bytes, max_side)
                    {
                        crate::cache::decoded_cache_insert(&key, pixels.clone());
                        return Some(pixels);
                    }
                    let bytes = get_with_retries(&url).ok()?;
                    if cache_images {
                        crate::cache::write_poster_bytes(&dir, &url, &bytes);
                    }
                    let pixels = decode_and_fit(&bytes, max_side)?;
                    crate::cache::decoded_cache_insert(&key, pixels.clone());
                    Some(pixels)
                }))
                .ok()
                .flatten();
                then(pixels);
            })
            .expect("spawn image thread");
    }

    /// Fire a GET + decode bypassing the LRU (refresh checks); `then`
    /// receives fresh pixels plus the raw bytes, or `None`.
    pub(crate) fn fetch_image_fresh(
        url: String,
        max_side: Option<u32>,
        then: impl FnOnce(Option<crate::cache::FreshImage>) + Send + 'static,
    ) {
        std::thread::Builder::new()
            .name(format!("image-refresh {}", short_url(&url)))
            .spawn(move || {
                let fresh = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    get_with_retries(&url).ok().and_then(|bytes| {
                        decode_and_fit(&bytes, max_side)
                            .map(|pixels| crate::cache::FreshImage { pixels, bytes })
                    })
                }))
                .ok()
                .flatten();
                then(fresh);
            })
            .expect("spawn image-refresh thread");
    }

    fn get_with_retries(url: &str) -> Result<Vec<u8>, FetchError> {
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

    fn get_once(url: &str) -> Result<Vec<u8>, FetchError> {
        let client = HTTP_CLIENT.as_ref().map_err(|e| {
            FetchError::new(None, format!("http client init failed: {e}"))
        })?;
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

    /// Decode with the `image` crate, downscaling so the longest side is at
    /// most `max_side` (`None` = full fidelity).
    fn decode_and_fit(
        bytes: &[u8],
        max_side: Option<u32>,
    ) -> Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> {
        let img = image::load_from_memory(bytes).ok()?;
        let img = match max_side {
            Some(m) => {
                let (w, h) = (img.width(), img.height());
                let longest = w.max(h);
                if longest > m && longest > 0 {
                    let scale = m as f32 / longest as f32;
                    img.resize(
                        ((w as f32 * scale).round() as u32).max(1),
                        ((h as f32 * scale).round() as u32).max(1),
                        image::imageops::FilterType::Triangle,
                    )
                } else {
                    img
                }
            }
            None => img,
        };
        let rgba = img.into_rgba8();
        let (w, h) = (rgba.dimensions().0, rgba.dimensions().1);
        Some(slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
            rgba.as_raw(),
            w,
            h,
        ))
    }

    fn short_url(url: &str) -> String {
        url.chars().take(48).collect()
    }
}

pub use imp::{fetch_bytes, fetch_image};
pub(crate) use imp::fetch_image_fresh;
#[cfg(target_os = "android")]
pub use imp::install_panic_hook;

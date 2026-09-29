//! Decoded-image LRU + on-disk poster cache pipeline.
//!
//! Extracted from the app crate so poster loading and the disk cache can be
//! reused by `nova-player` without pulling in the whole app. Desktop builds own
//! the full derivative/re-encode pipeline; Android shares the decoded LRU and
//! the raw-byte disk tier (its own decode path lives in `crate::net`).

#[allow(unused_imports)]
use nova_config::{
    active_cache_settings, fnv1a, poster_cache_dir, CacheImageFormat, CacheSettings,
    IMAGE_DOWNSCALE_MAX,
};
use slint::{Rgba8Pixel, SharedPixelBuffer};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
#[cfg(not(target_os = "android"))]
#[cfg(not(target_os = "android"))]
use std::time::Duration;
use std::fs;

/// Set the decoded-image LRU byte budget (mirrors `lru_cache_mb`).
pub fn set_decoded_cache_budget(mb: u32) {
    DECODED_POSTERS.lock().unwrap().max_bytes = mb as usize * 1024 * 1024;
}

/// Longest side for grid/library/detail display images (3x hidpi headroom
/// on 160px cards; ~8x under typical originals).
pub const DISPLAY_POSTER_SIDE: u32 = 480;
/// Unload hysteresis beyond the fetch window, in cards. The Slint grids
/// report a fetch window of visible ±2 rows; posters are blanked only
/// outside `[first - H, last + H]`, so just-preloaded rows above/below the
/// viewport are never immediately evicted on scroll jitter. Sized to match
/// the Slint `> 48 cards` report gate.
pub const UNLOAD_EXTRA_CARDS: usize = 48;
/// Longest side for episode-list thumbnails (112px display).
pub const EPISODE_THUMB_SIDE: u32 = 360;
/// Longest side for the detail-page backdrop banner (260px tall, full-bleed).
pub const DETAIL_BACKDROP_SIDE: u32 = 960;

/// Synchronously return cached backdrop pixels for `url` when the decoded
/// LRU already holds the `DETAIL_BACKDROP_SIDE` derivative. Lets detail
/// opens paint instantly on reentry instead of flashing through the
/// placeholder + an async fetch round-trip.
pub fn backdrop_pixels_cached(url: &str) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    if url.is_empty() {
        return None;
    }
    decoded_cache_get(&sized_cache_key(url, Some(DETAIL_BACKDROP_SIDE)))
}

/// Cache key for a display-sized derivative: the plain URL serves
/// full-fidelity buffers, suffixed keys serve downscaled ones. The suffix
/// keeps small buffers from ever shadowing the full-res entry the player
/// backdrop and re-encode path read.
pub fn sized_cache_key(url: &str, max_side: Option<u32>) -> String {
    match max_side {
        Some(m) => format!("{url}#{m}"),
        None => url.to_string(),
    }
}

/// Downscale `pixels` so the longest side is at most `max_side`, for
/// display purposes. Returns the original buffer unchanged when already
/// small enough, and on any failure. Triangle is cheap and plenty for
/// downscaling. Desktop only; Android decodes in `net`.
#[cfg(not(target_os = "android"))]
pub fn downscale_for_display(
    pixels: &SharedPixelBuffer<Rgba8Pixel>,
    max_side: u32,
) -> SharedPixelBuffer<Rgba8Pixel> {
    let (w, h) = (pixels.width(), pixels.height());
    let longest = w.max(h);
    if longest <= max_side || longest == 0 {
        return pixels.clone();
    }
    let scale = max_side as f32 / longest as f32;
    let (nw, nh) = (
        ((w as f32 * scale).round() as u32).max(1),
        ((h as f32 * scale).round() as u32).max(1),
    );
    let raw = match image::RgbaImage::from_raw(w, h, pixels.as_bytes().to_vec()) {
        Some(raw) => raw,
        None => return pixels.clone(),
    };
    let resized =
        image::DynamicImage::ImageRgba8(raw).resize(nw, nh, image::imageops::FilterType::Triangle);
    SharedPixelBuffer::clone_from_slice(resized.as_bytes(), nw, nh)
}

/// Return freed heap pages to the OS. On glibc this releases fully-free
/// top chunks, countering the classic "first load stays fat, restart is
/// lean" symptom. On Linux desktop builds the global allocator is jemalloc
/// (see main.rs), whose decay-based purge handles this on its own — the
/// trim then degrades to a cheap no-op against glibc's (nearly unused)
/// arenas, so the call sites stay unconditional.
#[cfg(target_os = "linux")]
pub fn trim_heap() {
    // SAFETY: malloc_trim(0) only releases fully-free pages.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn trim_heap() {}

/// File name for a display-sized derivative: same URL hash as the original
/// plus the target size, so it never collides with `<hash>.img` (and stays
/// invisible to the `*.img` re-encode sweep). No `.cfg` sidecar: the size
/// is baked into the name. Desktop only (web relies on the browser cache).
#[cfg(not(target_os = "android"))]
pub fn display_file_name(url: &str, max_side: u32) -> String {
    format!("{:016x}.d{max_side}.jpg", fnv1a(url.as_bytes()))
}

/// Read a cached display-sized derivative. Desktop only.
#[cfg(not(target_os = "android"))]
fn read_display_poster(url: &str, max_side: u32) -> Option<Vec<u8>> {
    fs::read(poster_cache_dir().join(display_file_name(url, max_side))).ok()
}

/// Persist a display-sized derivative as JPEG (best-effort cache; failures
/// are silent). Desktop only.
#[cfg(not(target_os = "android"))]
fn write_display_poster(url: &str, max_side: u32, pixels: &SharedPixelBuffer<Rgba8Pixel>) {
    let (w, h) = (pixels.width(), pixels.height());
    let raw = match image::RgbaImage::from_raw(w, h, pixels.as_bytes().to_vec()) {
        Some(raw) => raw,
        None => return,
    };
    let rgb = image::DynamicImage::ImageRgba8(raw).to_rgb8();
    let mut out: Vec<u8> = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80);
    if image::ImageEncoder::write_image(enc, &rgb, w, h, image::ExtendedColorType::Rgb8).is_err()
    {
        return;
    }
    let dir = poster_cache_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(display_file_name(url, max_side));
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, &out).and_then(|_| fs::rename(&tmp, &path)).is_err() {
        eprintln!("nova: could not write display poster for {url}");
    }
}

/// Display-sized pixels for `url`: sized-LRU hit → small-file disk hit →
/// full pipeline → downscale → populate both caches. `None` = full fidelity
/// (LRU full entry only; never reads or writes small files) — used for the
/// episode-pick player backdrop. Desktop only; the `fetch_image` wrappers on
/// both targets route through the same cascade shape.
#[cfg(not(target_os = "android"))]
pub fn display_pixels(
    url: &str,
    max_side: Option<u32>,
) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let key = sized_cache_key(url, max_side);
    if let Some(pixels) = decoded_cache_get(&key) {
        return Some(pixels);
    }
    if let Some(m) = max_side
        && active_cache_settings().cache_images
        && let Some(bytes) = read_display_poster(url, m)
        && let Ok(pixels) = decode_image_bytes(&bytes)
    {
        decoded_cache_insert(&key, pixels.clone());
        return Some(pixels);
    }
    // Full pipeline, deliberately NOT via poster_pixels: grid loads must
    // never insert full-res buffers into the shared LRU (it serves genuine
    // full-res consumers — episode backdrops, detail source, re-encode).
    let bytes = original_bytes(url)?;
    let pixels = decode_image_bytes(&bytes).ok()?;
    // Preserve lazy re-encode semantics: the hook sees every load, as it
    // did when this path still ran through poster_pixels.
    maybe_rewrite_cache_as_encoded(url, &pixels);
    match max_side {
        Some(m) => {
            let small = downscale_for_display(&pixels, m);
            decoded_cache_insert(&key, small.clone());
            if active_cache_settings().cache_images {
                write_display_poster(url, m, &small);
            }
            Some(small)
        }
        // Full fidelity: poster_pixels owns the full-tier LRU path.
        None => poster_pixels(url).ok(),
    }
}

/// Decode raw image bytes into a Slint pixel buffer (desktop only;
/// decodes with the browser via net::fetch_image).
#[cfg(not(target_os = "android"))]
pub fn decode_image_bytes(
    bytes: &[u8],
) -> Result<SharedPixelBuffer<Rgba8Pixel>, Box<dyn std::error::Error + Send + Sync>> {
    let dynamic_img = image::load_from_memory(bytes)?;
    // Move (not copy) when the decoded image is already RGBA8; one full-size
    // transient fewer per poster.
    let rgba = dynamic_img.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(SharedPixelBuffer::clone_from_slice(
        rgba.as_raw(),
        width,
        height,
    ))
}

/// Poster pixels for `url`: try the on-disk cache under `~/.cache/nova/posters`
/// first; on a miss, download the image once and store its raw bytes so later
/// runs never refetch the same poster. Bounded by a timeout so one
/// unresponsive image host cannot stall a poster worker forever; failures are
/// swallowed by the caller and the card simply keeps its placeholder.
/// A decoded image (poster / episode thumbnail) in the runtime LRU cache.
struct ImageCacheEntry {
    pixels: SharedPixelBuffer<Rgba8Pixel>,
    /// Approximate footprint (width * height * 4), used for the byte budget.
    bytes: usize,
    /// Monotonic last-use stamp (LRU eviction).
    last_use: u64,
}

/// Byte-budgeted LRU of decoded images keyed by URL. Decodes are expensive
/// (JXL in particular), so re-renders reuse them — but the cache is bounded
/// by `max_bytes` and evicts least-recently-used entries instead of letting
/// RAM grow without limit.
struct DecodedImageCache {
    map: HashMap<String, ImageCacheEntry>,
    total_bytes: usize,
    max_bytes: usize,
    clock: u64,
}

impl DecodedImageCache {
    fn touch_get(&mut self, key: &str) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
        self.clock = self.clock.wrapping_add(1);
        let entry = self.map.get_mut(key)?;
        entry.last_use = self.clock;
        Some(entry.pixels.clone())
    }

    fn contains(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    fn insert(&mut self, key: String, pixels: SharedPixelBuffer<Rgba8Pixel>) {
        let bytes = (pixels.width() as usize)
            .saturating_mul(pixels.height() as usize)
            .saturating_mul(core::mem::size_of::<Rgba8Pixel>());
        if bytes > self.max_bytes {
            return; // a single image this big isn't worth caching
        }
        if let Some(old) = self.map.remove(&key) {
            self.total_bytes = self.total_bytes.saturating_sub(old.bytes);
        }
        self.clock = self.clock.wrapping_add(1);
        self.map.insert(
            key,
            ImageCacheEntry {
                pixels: pixels.clone(),
                bytes,
                last_use: self.clock,
            },
        );
        self.total_bytes += bytes;
        self.evict();
    }

    fn evict(&mut self) {
        while self.total_bytes > self.max_bytes && !self.map.is_empty() {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.last_use)
                .map(|(k, _)| k.clone());
            if let Some(key) = oldest
                && let Some(entry) = self.map.remove(&key)
            {
                self.total_bytes = self.total_bytes.saturating_sub(entry.bytes);
            }
        }
    }

    /// Drop every decoded image (used by "Clear cache").
    fn clear(&mut self) {
        self.map.clear();
        self.total_bytes = 0;
    }
}

static DECODED_POSTERS: LazyLock<Mutex<DecodedImageCache>> = LazyLock::new(|| {
    Mutex::new(DecodedImageCache {
        map: HashMap::new(),
        total_bytes: 0,
        max_bytes: 64 * 1024 * 1024,
        clock: 0,
    })
});

pub fn decoded_cache_get(url: &str) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    DECODED_POSTERS.lock().unwrap().touch_get(url)
}

pub fn decoded_cache_contains(url: &str) -> bool {
    DECODED_POSTERS.lock().unwrap().contains(url)
}

pub fn decoded_cache_insert(url: &str, pixels: SharedPixelBuffer<Rgba8Pixel>) {
    DECODED_POSTERS
        .lock()
        .unwrap()
        .insert(url.to_string(), pixels);
}

/// Drop all decoded images from memory (used by "Clear cache"; disk is
/// wiped separately — in-flight downloads may repopulate either after).
pub fn decoded_cache_clear() {
    DECODED_POSTERS.lock().unwrap().clear();
}

/// Freshly downloaded image that bypassed every cache tier (used for
/// refresh checks: compare against the cached pixels before swapping).
pub struct FreshImage {
    pub(crate) pixels: SharedPixelBuffer<Rgba8Pixel>,
    /// Raw download bytes (persisted as the new on-disk original when the
    /// pixels changed; unused on web).
    pub(crate) bytes: Vec<u8>,
}

/// Download + decode `url` bypassing every cache tier (memory LRU and, on
/// desktop, the disk files), downscaled to `max_side`. Nothing is cached —
/// the caller decides what to do with the pixels. Desktop only.
#[cfg(not(target_os = "android"))]
pub fn download_image_fresh(url: &str, max_side: Option<u32>) -> Option<FreshImage> {
    // Always downloads: unlike `original_bytes`, the disk cache is
    // deliberately not consulted, so same-URL art changes are detected.
    let bytes = HTTP_CLIENT.get(url).send().ok()?.bytes().ok()?.to_vec();
    let pixels = decode_image_bytes(&bytes).ok()?;
    let pixels = match max_side {
        Some(m) => downscale_for_display(&pixels, m),
        None => pixels,
    };
    Some(FreshImage { pixels, bytes })
}
/// Byte-identical decoded pixels (same dimensions and RGBA bytes).
pub fn image_buffers_equal(
    a: &SharedPixelBuffer<Rgba8Pixel>,
    b: &SharedPixelBuffer<Rgba8Pixel>,
) -> bool {
    a.width() == b.width() && a.height() == b.height() && a.as_bytes() == b.as_bytes()
}

/// Refresh check for one image URL: fetch fresh bytes bypassing every cache
/// tier and swap them in only when the decoded pixels actually differ from
/// what's cached. `then` receives the new pixels on change (already stored
/// in every tier) and `None` when unchanged or on any failure — so callers
/// repaint only on real changes and never flash a placeholder for
/// identical art. The download runs off the UI thread (desktop worker) or
/// via the browser fetch (web); `then` inherits that context, so UI updates
/// must be marshalled to the event loop by the caller.
pub fn refresh_image_if_changed(
    url: String,
    max_side: Option<u32>,
    then: impl FnOnce(Option<SharedPixelBuffer<Rgba8Pixel>>) + Send + 'static,
) {
    let key = sized_cache_key(&url, max_side);
    crate::net::fetch_image_fresh(url.clone(), max_side, move |fresh| {
        let Some(fresh) = fresh else {
            // Download or decode failed: keep serving the cached art. Logged
            // (unlike unchanged images) so throttled hosts and dead URLs are
            // visible instead of silently stale.
            eprintln!("nova: image refresh failed: {url}");
            then(None);
            return;
        };
        let changed = match decoded_cache_get(&key) {
            Some(cached) => !image_buffers_equal(&cached, &fresh.pixels),
            // Nothing cached: adopt the fresh pixels (first paint path).
            None => true,
        };
        if !changed {
            then(None);
            return;
        }
        decoded_cache_insert(&key, fresh.pixels.clone());
        #[cfg(not(target_os = "android"))]
        {
            // Mirror the `display_pixels` write path (derivative + raw
            // original). The lazy re-encode sweep picks the new original
            // up via its config sidecar, as with any fresh download.
            if active_cache_settings().cache_images {
                if let Some(m) = max_side {
                    write_display_poster(&url, m, &fresh.pixels);
                }
                write_cached_poster(&url, &fresh.bytes);
            }
        }
        #[cfg(target_os = "android")]
        {
            // Same durability as desktop (raw original bytes only — no
            // display derivatives or re-encode on Android).
            if active_cache_settings().cache_images {
                write_poster_bytes(&poster_cache_dir(), &url, &fresh.bytes);
            }
        }
        eprintln!("nova: image refreshed (pixels changed): {url}");
        then(Some(fresh.pixels));
    });
}
/// Target dimensions when downscaling before a cache write; None when the
/// image already fits.
#[cfg(not(target_os = "android"))]
pub fn downscale_dims(w: u32, h: u32) -> Option<(u32, u32)> {
    let longest = w.max(h);
    if longest <= IMAGE_DOWNSCALE_MAX {
        return None;
    }
    let scale = IMAGE_DOWNSCALE_MAX as f64 / longest as f64;
    Some((
        ((w as f64 * scale).round() as u32).max(1),
        ((h as f64 * scale).round() as u32).max(1),
    ))
}

/// Re-encode `rgba` (optionally downscaled first) to the configured cache
/// format/quality. Returns the encoded bytes, or None when re-encoding is
/// disabled or encoding fails (callers then keep the raw bytes).
#[cfg(not(target_os = "android"))]
pub fn encode_for_cache(
    settings: &CacheSettings,
    rgba: &SharedPixelBuffer<Rgba8Pixel>,
) -> Option<Vec<u8>> {
    if !settings.enabled {
        return None;
    }
    let (w, h) = (rgba.width(), rgba.height());
    let target = downscale_dims(w, h).unwrap_or((w, h));

    let mut buf = rgba.clone();
    let raw = buf.make_mut_bytes().to_vec();
    let source = image::RgbaImage::from_raw(w, h, raw)?;
    let img = if target != (w, h) {
        image::DynamicImage::ImageRgba8(source).resize(
            target.0,
            target.1,
            image::imageops::FilterType::Triangle,
        )
    } else {
        image::DynamicImage::ImageRgba8(source)
    };

    let mut out: Vec<u8> = Vec::new();
    match settings.format {
        CacheImageFormat::Jpeg => {
            let rgb = img.to_rgb8();
            let enc =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, settings.quality);
            image::ImageEncoder::write_image(
                enc,
                &rgb,
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
            .ok()?;
        }
        CacheImageFormat::Webp => {
            // Lossy WebP via libwebp bindings (image's own WebP encoder is
            // lossless-only), at the configured quality.
            let rgba = img.to_rgba8();
            let encoder = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height());
            let memory = encoder.encode(settings.quality as f32);
            out = memory.to_vec();
        }
    }
    Some(out)
}

/// Encode a freshly decoded poster to the configured format (when enabled)
/// and atomically replace the disk cache entry, tagging it with a sidecar
/// holding the config key so it is only re-encoded when settings change.
#[cfg(not(target_os = "android"))]
fn maybe_rewrite_cache_as_encoded(url: &str, rgba: &SharedPixelBuffer<Rgba8Pixel>) {
    let settings = active_cache_settings();
    if !settings.cache_images || !settings.enabled || !settings.lazy_reencode {
        return;
    }
    let path = poster_cache_path_in(&poster_cache_dir(), url);
    let meta = path.with_extension("cfg");
    let key = settings.config_key();
    if fs::read_to_string(&meta)
        .map(|s| s.trim() == key)
        .unwrap_or(false)
    {
        return; // already stored with the current config
    }
    let Some(bytes) = encode_for_cache(&settings, rgba) else {
        return; // keep raw bytes as-is
    };
    write_poster_bytes(&poster_cache_dir(), url, &bytes);
    let _ = fs::write(&meta, key);
}

/// Rewrite every image file in the cache directory `dir` to `settings`'s
/// format/quality. Entries whose sidecar already carries the current config
/// key are left untouched (so re-running is cheap and idempotent). Returns
/// how many files were rewritten.
#[cfg(not(target_os = "android"))]
pub fn rewrite_cache_dir_to_format(dir: &Path, settings: &CacheSettings) -> usize {
    if !settings.cache_images || !settings.enabled {
        return 0;
    }
    let key = settings.config_key();
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut rewritten = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("img") {
            continue;
        }
        // The `.cfg` sidecar stores the config key that produced this file.
        let meta = path.with_extension("cfg");
        if fs::read_to_string(&meta)
            .map(|s| s.trim() == key)
            .unwrap_or(false)
        {
            continue; // already encoded with the current config
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(pixels) = decode_image_bytes(&bytes) else {
            continue;
        };
        let Some(encoded) = encode_for_cache(settings, &pixels) else {
            continue; // keep the stored bytes as-is
        };
        let tmp = path.with_extension("tmp");
        if fs::write(&tmp, &encoded)
            .and_then(|_| fs::rename(&tmp, &path))
            .is_err()
        {
            continue;
        }
        let _ = fs::write(&meta, &key);
        rewritten += 1;
    }
    rewritten
}
#[cfg(not(target_os = "android"))]
/// Shared blocking HTTP client: connection pooling across poster downloads
/// (a fresh client per request, as before, skips pooling and churns a
/// runtime per image). Desktop only.
#[cfg(not(target_os = "android"))]
static HTTP_CLIENT: LazyLock<reqwest::blocking::Client> = LazyLock::new(|| {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("http client builds")
});

/// Original (full-resolution) bytes for `url`: disk cache hit, else
/// download once and store the raw bytes for later runs (unless on-disk
/// image caching is disabled in settings — then always download). Pure byte
/// layer — no decoding, no memory caches. Desktop only.
#[cfg(not(target_os = "android"))]
fn original_bytes(url: &str) -> Option<Vec<u8>> {
    let cache_images = active_cache_settings().cache_images;
    if cache_images
        && let Some(bytes) = read_poster_bytes(&poster_cache_dir(), url)
    {
        return Some(bytes);
    }
    let bytes = HTTP_CLIENT.get(url).send().ok()?.bytes().ok()?;
    if cache_images {
        write_cached_poster(url, &bytes);
    }
    Some(bytes.to_vec())
}

#[cfg(not(target_os = "android"))]
pub fn poster_pixels(
    url: &str,
) -> Result<SharedPixelBuffer<Rgba8Pixel>, Box<dyn std::error::Error + Send + Sync>> {
    // In-memory LRU cache: avoids re-decoding posters seen earlier this run.
    if let Some(pixels) = decoded_cache_get(url) {
        return Ok(pixels);
    }

    let bytes = original_bytes(url)
        .ok_or_else(|| -> Box<dyn std::error::Error + Send + Sync> {
            "poster download failed".into()
        })?;
    let pixels = decode_image_bytes(&bytes)?;

    // If image re-encoding is enabled, store the encoded (optionally
    // downscaled) bytes on disk and tag the entry with the config key.
    maybe_rewrite_cache_as_encoded(url, &pixels);

    decoded_cache_insert(url, pixels.clone());
    Ok(pixels)
}
/// Cache file name for a poster URL (shared with the testable core helpers).
fn poster_file_name(url: &str) -> String {
    format!("{:016x}.img", fnv1a(url.as_bytes()))
}

/// Core: path of the poster cache file for `url` inside `dir`.
pub fn poster_cache_path_in(dir: &Path, url: &str) -> PathBuf {
    dir.join(poster_file_name(url))
}

/// Core: persist raw poster bytes under `dir`.
pub fn write_poster_bytes(dir: &Path, url: &str, bytes: &[u8]) {
    if fs::create_dir_all(dir).is_err() {
        eprintln!("nova: could not create poster cache dir {:?}", dir);
        return;
    }
    let path = poster_cache_path_in(dir, url);
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, bytes)
        .and_then(|_| fs::rename(&tmp, &path))
        .is_err()
    {
        eprintln!("nova: could not write poster cache for {url}");
    }
}

/// Core: read raw poster bytes cached under `dir`.
pub fn read_poster_bytes(dir: &Path, url: &str) -> Option<Vec<u8>> {
    fs::read(poster_cache_path_in(dir, url)).ok()
}

/// Store the raw bytes of a freshly downloaded poster for later runs.
#[cfg(not(target_os = "android"))]
fn write_cached_poster(url: &str, bytes: &[u8]) {
    write_poster_bytes(&poster_cache_dir(), url, bytes);
}

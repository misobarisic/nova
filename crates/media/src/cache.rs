//! Decoded-image LRU + on-disk poster cache pipeline.
//!
//! Extracted from the app crate so poster loading and the disk cache can be
//! reused by `nova-player` without pulling in the whole app. Encoding, atomic
//! file replacement and maintenance are shared by desktop and Android; only
//! HTTP transport and desktop display derivatives are platform-specific.

use nova_config::{
    CacheImageFormat, CacheSettings, IMAGE_DOWNSCALE_MAX, active_cache_settings, fnv1a,
    poster_cache_dir,
};
use slint::{Rgba8Pixel, SharedPixelBuffer};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, Weak};

/// Set the decoded-image LRU byte budget (mirrors `lru_cache_mb`).
pub fn set_decoded_cache_budget(mb: u32) {
    let mut cache = DECODED_POSTERS.lock().unwrap();
    cache.max_bytes = mb as usize * 1024 * 1024;
    cache.evict();
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

fn source_url(key: &str) -> &str {
    key.rsplit_once('#')
        .filter(|(_, size)| size.parse::<u32>().is_ok())
        .map_or(key, |(url, _)| url)
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
/// downscaling. Shared by both platform pipelines.
pub fn downscale_for_display(
    pixels: &SharedPixelBuffer<Rgba8Pixel>,
    max_side: u32,
) -> SharedPixelBuffer<Rgba8Pixel> {
    let (w, h) = (pixels.width(), pixels.height());
    let longest = w.max(h);
    if longest <= max_side || longest == 0 {
        return pixels.clone();
    }
    let raw = match image::RgbaImage::from_raw(w, h, pixels.as_bytes().to_vec()) {
        Some(raw) => raw,
        None => return pixels.clone(),
    };
    // Fit once, then use the image's actual dimensions. Fitting a second
    // time into pre-rounded bounds can drop a row/column; labelling those
    // bytes with the larger bounds makes Skia reject an otherwise valid image.
    let resized = image::DynamicImage::ImageRgba8(raw).resize(
        max_side,
        max_side,
        image::imageops::FilterType::Triangle,
    );
    let (nw, nh) = (resized.width(), resized.height());
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
/// is baked into the name. Desktop only; Android uses the shared source tier.
#[cfg(not(target_os = "android"))]
pub fn display_file_name(url: &str, max_side: u32) -> String {
    format!("{:016x}.d{max_side}.jpg", fnv1a(url.as_bytes()))
}

/// Read and publish a derivative under its source lock so conversion cannot
/// invalidate it between decoding and inserting a verified memory entry.
#[cfg(not(target_os = "android"))]
fn read_display_pixels(url: &str, max_side: u32) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let dir = poster_cache_dir();
    let lock = entry_lock(&poster_cache_path_in(&dir, url));
    let _guard = lock.lock().unwrap();
    let bytes = fs::read(dir.join(display_file_name(url, max_side))).ok()?;
    let pixels = decode_image_bytes(&bytes).ok()?;
    decoded_cache_insert(&sized_cache_key(url, Some(max_side)), pixels.clone());
    Some(pixels)
}

/// Persist a display-sized derivative as JPEG (best-effort cache; failures
/// are silent). Desktop only.
#[cfg(not(target_os = "android"))]
fn write_display_poster(
    url: &str,
    max_side: u32,
    pixels: &SharedPixelBuffer<Rgba8Pixel>,
    source_bytes: &[u8],
) {
    let _permit = EncodingPermit::acquire();
    let (w, h) = (pixels.width(), pixels.height());
    let raw = match image::RgbaImage::from_raw(w, h, pixels.as_bytes().to_vec()) {
        Some(raw) => raw,
        None => return,
    };
    let rgb = image::DynamicImage::ImageRgba8(raw).to_rgb8();
    let mut out: Vec<u8> = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80);
    if image::ImageEncoder::write_image(enc, &rgb, w, h, image::ExtendedColorType::Rgb8).is_err() {
        return;
    }
    let dir = poster_cache_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let source = poster_cache_path_in(&dir, url);
    let lock = entry_lock(&source);
    let _guard = lock.lock().unwrap();
    if fs::read(&source).ok().as_deref() != Some(source_bytes) {
        return;
    }
    let path = dir.join(display_file_name(url, max_side));
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, &out)
        .and_then(|_| fs::rename(&tmp, &path))
        .is_err()
    {
        eprintln!("nova: could not write display poster for {url}");
    }
}

/// Display-sized pixels for `url`: sized-LRU hit → small-file disk hit →
/// full pipeline → downscale → populate both caches. `None` = full fidelity
/// (LRU full entry only; never reads or writes small files) — used for the
/// episode-pick player backdrop. Desktop only; the `fetch_image` wrappers on
/// both targets route through the same cascade shape.
#[cfg(not(target_os = "android"))]
pub fn display_pixels(url: &str, max_side: Option<u32>) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    ensure_lazy_conversion(url);
    let key = sized_cache_key(url, max_side);
    if let Some(pixels) = decoded_cache_get(&key) {
        return Some(pixels);
    }
    if let Some(m) = max_side
        && active_cache_settings().cache_images
        && let Some(pixels) = read_display_pixels(url, m)
    {
        return Some(pixels);
    }
    // Full pipeline, deliberately NOT via poster_pixels: grid loads must
    // never insert full-res buffers into the shared LRU (it serves genuine
    // full-res consumers — episode backdrops, detail source, re-encode).
    let bytes = original_bytes(url)?;
    let pixels = decode_image_bytes(&bytes).ok()?;
    // Preserve lazy re-encode semantics: the hook sees every load, as it
    // did when this path still ran through poster_pixels.
    ensure_lazy_conversion(url);
    match max_side {
        Some(m) => {
            let small = downscale_for_display(&pixels, m);
            cache_source_pixels(
                &poster_cache_dir(),
                url,
                &key,
                &small,
                &bytes,
                active_cache_settings().cache_images,
            );
            if active_cache_settings().cache_images {
                write_display_poster(url, m, &small, &bytes);
            }
            Some(small)
        }
        // Full fidelity: poster_pixels owns the full-tier LRU path.
        None => poster_pixels(url).ok(),
    }
}

/// Decode source bytes into a pixel buffer on a worker on either platform.
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
    /// Sidecar observed when these pixels were decoded. Lazy mode rejects
    /// unverified hits and lets the worker convert the full disk source.
    disk_config: Option<String>,
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
        let settings = active_cache_settings();
        if settings.cache_images
            && settings.enabled
            && settings.lazy_reencode
            && entry.disk_config.as_deref().map(str::trim) != Some(settings.config_key().as_str())
        {
            return None;
        }
        entry.last_use = self.clock;
        Some(entry.pixels.clone())
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
        let disk_config = fs::read_to_string(
            poster_cache_path_in(&poster_cache_dir(), source_url(&key)).with_extension("cfg"),
        )
        .ok();
        self.map.insert(
            key,
            ImageCacheEntry {
                pixels: pixels.clone(),
                bytes,
                last_use: self.clock,
                disk_config,
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
        max_bytes: 128 * 1024 * 1024,
        clock: 0,
    })
});

pub fn decoded_cache_get(url: &str) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    DECODED_POSTERS.lock().unwrap().touch_get(url)
}

pub fn decoded_cache_contains(url: &str) -> bool {
    decoded_cache_get(url).is_some()
}

pub fn decoded_cache_insert(url: &str, pixels: SharedPixelBuffer<Rgba8Pixel>) {
    DECODED_POSTERS
        .lock()
        .unwrap()
        .insert(url.to_string(), pixels);
}

// A sweep can replace a source while a worker decodes its old snapshot. Only
// publish pixels if that source is still current; otherwise the next load must
// decode the replacement rather than stamp old pixels with its new sidecar.
fn cache_source_pixels(
    dir: &Path,
    url: &str,
    key: &str,
    pixels: &SharedPixelBuffer<Rgba8Pixel>,
    source: &[u8],
    disk_enabled: bool,
) {
    let path = poster_cache_path_in(dir, url);
    let lock = entry_lock(&path);
    let _guard = lock.lock().unwrap();
    if !disk_enabled || fs::read(path).ok().as_deref() == Some(source) {
        decoded_cache_insert(key, pixels.clone());
    }
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
    let bytes = addons::http_get_blocking(url).ok()?;
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
        if active_cache_settings().cache_images {
            let _ = store_download(
                &poster_cache_dir(),
                &url,
                &fresh.bytes,
                &active_cache_settings(),
            );
        }
        let disk_enabled = active_cache_settings().cache_images;
        let source = disk_enabled
            .then(|| read_poster_bytes(&poster_cache_dir(), &url))
            .flatten();
        let pixels = if disk_enabled {
            source
                .as_ref()
                .and_then(|bytes| decode_image_bytes(bytes).ok())
                .map(|pixels| {
                    max_side.map_or_else(
                        || pixels.clone(),
                        |side| downscale_for_display(&pixels, side),
                    )
                })
                .unwrap_or(fresh.pixels)
        } else {
            fresh.pixels
        };
        cache_source_pixels(
            &poster_cache_dir(),
            &url,
            &key,
            &pixels,
            source.as_deref().unwrap_or(&fresh.bytes),
            disk_enabled,
        );
        eprintln!("nova: image refreshed (pixels changed): {url}");
        then(Some(pixels));
    });
}
/// Target dimensions when downscaling before a cache write; None when the
/// image already fits.
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
pub fn encode_for_cache(
    settings: &CacheSettings,
    rgba: &SharedPixelBuffer<Rgba8Pixel>,
) -> Option<Vec<u8>> {
    if !settings.enabled {
        return None;
    }
    let (w, h) = (rgba.width(), rgba.height());
    let _permit = EncodingPermit::acquire();
    let target = if settings.downscale {
        downscale_dims(w, h).unwrap_or((w, h))
    } else {
        (w, h)
    };

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
            let memory = encoder.encode_simple(false, settings.quality as f32).ok()?;
            out = memory.to_vec();
        }
    }
    // Native encoder failures must never replace valid stored bytes.
    image::load_from_memory(&out).ok()?;
    Some(out)
}

// Entry locks serialize downloads, lazy conversion and sweeps without retaining
// one mutex per URL forever. Readers see complete files through atomic rename.
static ENTRY_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static ENCODINGS: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());
static MAINTENANCE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

fn entry_lock(path: &Path) -> Arc<Mutex<()>> {
    let mut locks = ENTRY_LOCKS.lock().unwrap();
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(path.to_owned(), Arc::downgrade(&lock));
    lock
}

struct EncodingPermit;
impl EncodingPermit {
    fn acquire() -> Self {
        let mut count = ENCODINGS.0.lock().unwrap();
        while *count >= 2 {
            count = ENCODINGS.1.wait(count).unwrap();
        }
        *count += 1;
        Self
    }
}
impl Drop for EncodingPermit {
    fn drop(&mut self) {
        *ENCODINGS.0.lock().unwrap() -= 1;
        ENCODINGS.1.notify_one();
    }
}

/// One clear/rewrite job across platforms. Held until all atomic writes finish.
pub struct MaintenanceGuard;
impl MaintenanceGuard {
    pub fn try_acquire() -> Option<Self> {
        let mut busy = MAINTENANCE.0.lock().unwrap();
        if *busy {
            return None;
        }
        *busy = true;
        Some(Self)
    }
    fn acquire() -> Self {
        let mut busy = MAINTENANCE.0.lock().unwrap();
        while *busy {
            busy = MAINTENANCE.1.wait(busy).unwrap();
        }
        *busy = true;
        Self
    }
    pub fn clear(&self, dir: &Path) -> (u64, usize) {
        clear_cache_entries(dir)
    }
    pub fn rewrite(
        &self,
        dir: &Path,
        settings: &CacheSettings,
        cancel: &AtomicBool,
        progress: impl FnMut(RewriteProgress),
    ) -> RewriteProgress {
        rewrite_entries(dir, settings, cancel, progress)
    }
}
impl Drop for MaintenanceGuard {
    fn drop(&mut self) {
        *MAINTENANCE.0.lock().unwrap() = false;
        MAINTENANCE.1.notify_one();
    }
}

fn invalidate_entry(path: &Path) {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return;
    };
    let mut cache = DECODED_POSTERS.lock().unwrap();
    let keys: Vec<_> = cache
        .map
        .keys()
        .filter(|key| {
            let url = key
                .rsplit_once('#')
                .filter(|(_, size)| size.parse::<u32>().is_ok())
                .map_or(key.as_str(), |(url, _)| url);
            format!("{:016x}", fnv1a(url.as_bytes())) == stem
        })
        .cloned()
        .collect();
    for key in keys {
        if let Some(entry) = cache.map.remove(&key) {
            cache.total_bytes = cache.total_bytes.saturating_sub(entry.bytes);
        }
    }
    drop(cache);
    if let Some(dir) = path.parent()
        && let Ok(entries) = fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{stem}.d"))
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

// The caller holds the entry lock. Remove the old marker before replacing the
// image: a failed marker write must never claim raw/new bytes use an old config.
fn replace_entry(path: &Path, bytes: &[u8], key: Option<&str>) -> std::io::Result<()> {
    fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("img.tmp");
    fs::write(&tmp, bytes)?;
    let meta = path.with_extension("cfg");
    if meta.exists() {
        fs::remove_file(&meta)?;
    }
    if let Err(error) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    invalidate_entry(path);
    if let Some(key) = key {
        let tmp = meta.with_extension("cfg.tmp");
        fs::write(&tmp, key)?;
        fs::rename(tmp, meta)?;
    }
    Ok(())
}

/// Persist a valid full-resolution download, compressing independently of lazy
/// conversion. Encoding failure preserves the downloaded bytes.
pub fn store_download(
    dir: &Path,
    url: &str,
    bytes: &[u8],
    settings: &CacheSettings,
) -> std::io::Result<()> {
    if !settings.cache_images {
        return Ok(());
    }
    let pixels = decode_image_bytes(bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let encoded = encode_for_cache(settings, &pixels);
    let path = poster_cache_path_in(dir, url);
    let lock = entry_lock(&path);
    let _guard = lock.lock().unwrap();
    replace_entry(
        &path,
        encoded.as_deref().unwrap_or(bytes),
        encoded.as_ref().map(|_| settings.config_key()).as_deref(),
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RewriteProgress {
    pub processed: usize,
    pub total: usize,
    pub converted: usize,
    pub skipped: usize,
    pub failed: usize,
    pub cancelled: bool,
}

fn convert_entry(path: &Path, settings: &CacheSettings) -> std::io::Result<bool> {
    let lock = entry_lock(path);
    let _guard = lock.lock().unwrap();
    let key = settings.config_key();
    let bytes = fs::read(path)?;
    let pixels = decode_image_bytes(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if fs::read_to_string(path.with_extension("cfg")).is_ok_and(|s| s.trim() == key) {
        return Ok(false);
    }
    let encoded = encode_for_cache(settings, &pixels)
        .ok_or_else(|| std::io::Error::other("image encoding failed"))?;
    replace_entry(path, &encoded, Some(&key))?;
    Ok(true)
}

/// Check disk configuration before *any* decoded/derivative cache hit. The
/// source is always the .img entry, never the caller's display thumbnail.
pub fn ensure_lazy_conversion_in(dir: &Path, url: &str, settings: &CacheSettings) {
    if settings.cache_images && settings.enabled && settings.lazy_reencode {
        let path = poster_cache_path_in(dir, url);
        if fs::read_to_string(path.with_extension("cfg"))
            .is_ok_and(|key| key.trim() == settings.config_key())
        {
            return;
        }
        let _ = convert_entry(&path, settings);
    }
}
pub fn ensure_lazy_conversion(url: &str) {
    ensure_lazy_conversion_in(&poster_cache_dir(), url, &active_cache_settings());
}

/// Snapshot settings are supplied by the job owner. Stop is checked between
/// entries; an in-progress atomic replacement always completes first.
fn rewrite_entries(
    dir: &Path,
    settings: &CacheSettings,
    cancel: &AtomicBool,
    mut progress: impl FnMut(RewriteProgress),
) -> RewriteProgress {
    let mut result = RewriteProgress::default();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            result.failed = usize::from(error.kind() != std::io::ErrorKind::NotFound);
            progress(result);
            return result;
        }
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "img"))
        .collect();
    paths.sort();
    result.total = paths.len();
    progress(result);
    for path in paths {
        if cancel.load(Ordering::Acquire) {
            result.cancelled = true;
            break;
        }
        if !settings.cache_images || !settings.enabled {
            result.skipped += 1;
        } else {
            match convert_entry(&path, settings) {
                Ok(true) => result.converted += 1,
                Ok(false) => result.skipped += 1,
                Err(_) => result.failed += 1,
            }
        }
        result.processed += 1;
        progress(result);
    }
    progress(result);
    result
}

/// Serialize background maintenance for non-UI callers too. UI owners acquire
/// the guard before spawning and call `MaintenanceGuard::rewrite` directly.
pub fn rewrite_cache_dir_with_progress(
    dir: &Path,
    settings: &CacheSettings,
    cancel: &AtomicBool,
    progress: impl FnMut(RewriteProgress),
) -> RewriteProgress {
    MaintenanceGuard::acquire().rewrite(dir, settings, cancel, progress)
}

/// Compatibility helper for callers that only need the converted count.
pub fn rewrite_cache_dir_to_format(dir: &Path, settings: &CacheSettings) -> usize {
    rewrite_cache_dir_with_progress(dir, settings, &AtomicBool::new(false), |_| {}).converted
}

/// Original (full-resolution) bytes for `url`: disk cache hit, else
/// download once and store original or compressed source bytes according to
/// settings. Validation prevents corrupt disk entries from blocking refetches.
/// Desktop HTTP transport; encoding and atomic storage are shared.
#[cfg(not(target_os = "android"))]
fn original_bytes(url: &str) -> Option<Vec<u8>> {
    let cache_images = active_cache_settings().cache_images;
    if cache_images
        && let Some(bytes) = read_poster_bytes(&poster_cache_dir(), url)
        && decode_image_bytes(&bytes).is_ok()
    {
        return Some(bytes);
    }
    // Share the pooled addon client and Android's retry/status policy instead
    // of treating a transient server error as permanently missing artwork.
    let bytes = addons::http_get_blocking(url).ok()?;
    if cache_images {
        write_cached_poster(url, &bytes);
        if let Some(stored) = read_poster_bytes(&poster_cache_dir(), url) {
            return Some(stored);
        }
    }
    Some(bytes.to_vec())
}

#[cfg(not(target_os = "android"))]
pub fn poster_pixels(
    url: &str,
) -> Result<SharedPixelBuffer<Rgba8Pixel>, Box<dyn std::error::Error + Send + Sync>> {
    ensure_lazy_conversion(url);
    // In-memory LRU cache: avoids re-decoding posters seen earlier this run.
    if let Some(pixels) = decoded_cache_get(url) {
        return Ok(pixels);
    }

    let bytes =
        original_bytes(url).ok_or_else(|| -> Box<dyn std::error::Error + Send + Sync> {
            "poster download failed".into()
        })?;
    let pixels = decode_image_bytes(&bytes)?;

    // If image re-encoding is enabled, store the encoded (optionally
    // downscaled) bytes on disk and tag the entry with the config key.
    ensure_lazy_conversion(url);

    cache_source_pixels(
        &poster_cache_dir(),
        url,
        url,
        &pixels,
        &bytes,
        active_cache_settings().cache_images,
    );
    Ok(pixels)
}
/// Full source first, then display fit. Android supplies its existing rustls
/// transport; the injectable download also tests new-image behavior without HTTP.
pub fn cached_pixels_with(
    dir: &Path,
    url: &str,
    max_side: Option<u32>,
    settings: &CacheSettings,
    download: impl FnOnce() -> Option<Vec<u8>>,
) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    ensure_lazy_conversion_in(dir, url, settings);
    let key = sized_cache_key(url, max_side);
    if let Some(pixels) = decoded_cache_get(&key) {
        return Some(pixels);
    }
    let cached = settings
        .cache_images
        .then(|| read_poster_bytes(dir, url))
        .flatten()
        .filter(|bytes| decode_image_bytes(bytes).is_ok());
    let bytes = match cached {
        Some(bytes) => bytes,
        None => {
            let bytes = download()?;
            if settings.cache_images {
                let _ = store_download(dir, url, &bytes, settings);
                read_poster_bytes(dir, url).unwrap_or(bytes)
            } else {
                bytes
            }
        }
    };
    let pixels = decode_image_bytes(&bytes).ok()?;
    let pixels = max_side.map_or_else(
        || pixels.clone(),
        |side| downscale_for_display(&pixels, side),
    );
    cache_source_pixels(dir, url, &key, &pixels, &bytes, settings.cache_images);
    Some(pixels)
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
    if decode_image_bytes(bytes).is_err() {
        return;
    }
    let path = poster_cache_path_in(dir, url);
    let lock = entry_lock(&path);
    let _guard = lock.lock().unwrap();
    if let Err(error) = replace_entry(&path, bytes, None) {
        eprintln!("nova: could not write poster cache for {url}: {error}");
    }
}

/// Core: read raw poster bytes cached under `dir`.
pub fn read_poster_bytes(dir: &Path, url: &str) -> Option<Vec<u8>> {
    fs::read(poster_cache_path_in(dir, url)).ok()
}

/// Store the raw bytes of a freshly downloaded poster for later runs.
#[cfg(not(target_os = "android"))]
fn write_cached_poster(url: &str, bytes: &[u8]) {
    let _ = store_download(&poster_cache_dir(), url, bytes, &active_cache_settings());
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;

/// Remove cache-owned files under their entry lock, including orphan markers.
/// Non-UI callers share the same maintenance gate as rewriting.
pub fn clear_poster_cache_dir(dir: &Path) -> (u64, usize) {
    MaintenanceGuard::acquire().clear(dir)
}

fn clear_cache_entries(dir: &Path) -> (u64, usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut bytes = 0u64;
    let mut files = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // All cache artifacts begin with the original URL hash. Taking the
        // same lock for images, derivatives, markers and temporaries prevents
        // a clear from tearing an in-flight atomic image/marker replacement.
        let Some(stem) = name.split('.').next() else {
            continue;
        };
        let lock = entry_lock(&dir.join(format!("{stem}.img")));
        let _guard = lock.lock().unwrap();
        if let Ok(meta) = fs::metadata(&path)
            && meta.is_file()
            && fs::remove_file(&path).is_ok()
        {
            bytes = bytes.saturating_add(meta.len());
            files += 1;
        }
    }
    decoded_cache_clear();
    (bytes, files)
}

/// Actual original/derivative/sidecar disk footprint; no estimate from pixels.
pub fn poster_cache_disk_usage(dir: &Path) -> (u64, usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return (0, 0);
    };
    entries
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .filter(|meta| meta.is_file())
        .fold((0u64, 0), |(bytes, count), meta| {
            (bytes.saturating_add(meta.len()), count + 1)
        })
}

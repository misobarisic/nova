//! One recipe-keyed decoded LRU and processed disk-variant pipeline on all platforms.
//! Downloads and source decodes are transient; only bounded, processed pixels enter
//! the cache. The transport lives in `net`, independent of this processing core.
use nova_config::{
    CacheImageFormat, CacheSettings, active_cache_settings, fnv1a, image_settings_revision,
    poster_cache_dir,
};
use serde::{Deserialize, Serialize};
use slint::{Rgba8Pixel, SharedPixelBuffer};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, LazyLock, Mutex, RwLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

type Pixels = SharedPixelBuffer<Rgba8Pixel>;
pub const DISPLAY_POSTER_SIDE: u32 = 480;
pub const EPISODE_THUMB_SIDE: u32 = 360;
pub const DETAIL_BACKDROP_SIDE: u32 = 960;
pub const UNLOAD_EXTRA_CARDS: usize = 48;

/// Semantic uses resolve preferences at the start of each load; fixed uses keep
/// their existing display sizes. Uses with identical recipes share pixels/files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ImageUse {
    DetailPoster,
    DetailBackdrop,
    EpisodeThumbnail,
    PlayerEpisodeBackdrop,
    Fixed(Option<u32>),
}
impl From<Option<u32>> for ImageUse {
    fn from(value: Option<u32>) -> Self {
        Self::Fixed(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImageRecipe {
    pub max_side: Option<u32>,
    pub compress: bool,
    pub format: Option<CacheImageFormat>,
    pub quality: u8,
}
impl ImageRecipe {
    pub fn resolve(image_use: impl Into<ImageUse>, settings: &CacheSettings) -> Self {
        let max_side = match image_use.into() {
            ImageUse::DetailPoster => settings.detail_poster_max_side,
            ImageUse::DetailBackdrop => settings.detail_backdrop_max_side,
            ImageUse::EpisodeThumbnail => settings.episode_thumbnail_max_side,
            ImageUse::PlayerEpisodeBackdrop => settings.player_episode_backdrop_max_side,
            ImageUse::Fixed(side) => side,
        };
        Self {
            max_side,
            compress: settings.enabled,
            format: settings.enabled.then_some(settings.format),
            quality: if settings.enabled {
                settings.quality.clamp(1, 100)
            } else {
                0
            },
        }
    }
    fn key(self) -> String {
        serde_json::to_string(&self).expect("image recipe is serializable")
    }
}
fn image_recipe_cache_key(url: &str, recipe: ImageRecipe) -> String {
    format!("{url}\u{1f}nova-image-v2:{}", recipe.key())
}
pub fn image_cache_key(url: &str, image_use: impl Into<ImageUse>) -> String {
    image_recipe_cache_key(
        url,
        ImageRecipe::resolve(image_use, &active_cache_settings()),
    )
}
pub fn sized_cache_key(url: &str, max_side: Option<u32>) -> String {
    image_cache_key(url, ImageUse::Fixed(max_side))
}
pub fn backdrop_pixels_cached(url: &str) -> Option<Pixels> {
    decoded_cache_get(&image_cache_key(url, ImageUse::DetailBackdrop))
}

struct ImageCacheEntry {
    pixels: Pixels,
    bytes: usize,
    last_use: u64,
}
struct DecodedImageCache {
    map: HashMap<String, ImageCacheEntry>,
    total_bytes: usize,
    max_bytes: usize,
    clock: u64,
}
impl DecodedImageCache {
    fn touch_get(&mut self, key: &str) -> Option<Pixels> {
        self.clock = self.clock.wrapping_add(1);
        let entry = self.map.get_mut(key)?;
        entry.last_use = self.clock;
        Some(entry.pixels.clone())
    }
    fn insert(&mut self, key: String, pixels: Pixels) {
        if let Some(old) = self.map.remove(&key) {
            self.total_bytes = self.total_bytes.saturating_sub(old.bytes);
        }
        let bytes = pixels.as_bytes().len();
        if bytes > self.max_bytes {
            return;
        }
        self.clock = self.clock.wrapping_add(1);
        self.map.insert(
            key,
            ImageCacheEntry {
                pixels,
                bytes,
                last_use: self.clock,
            },
        );
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        self.evict();
    }
    fn evict(&mut self) {
        while self.total_bytes > self.max_bytes {
            let Some(key) = self
                .map
                .iter()
                .min_by_key(|(_, entry)| entry.last_use)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(old) = self.map.remove(&key) {
                self.total_bytes = self.total_bytes.saturating_sub(old.bytes);
            }
        }
    }
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
pub fn set_decoded_cache_budget(mb: u32) {
    let mut cache = DECODED_POSTERS.lock().unwrap();
    cache.max_bytes = (mb as usize).saturating_mul(1024 * 1024);
    cache.evict();
}
pub fn decoded_cache_stats() -> (usize, usize) {
    let cache = DECODED_POSTERS.lock().unwrap();
    (cache.map.len(), cache.total_bytes)
}
pub fn decoded_cache_get(key: &str) -> Option<Pixels> {
    DECODED_POSTERS.lock().unwrap().touch_get(key)
}
pub fn decoded_cache_contains(key: &str) -> bool {
    decoded_cache_get(key).is_some()
}
pub fn decoded_cache_insert(key: &str, pixels: Pixels) {
    DECODED_POSTERS
        .lock()
        .unwrap()
        .insert(key.to_owned(), pixels);
}
pub fn decoded_cache_clear() {
    DECODED_POSTERS.lock().unwrap().clear();
}
pub fn image_buffers_equal(a: &Pixels, b: &Pixels) -> bool {
    a.width() == b.width() && a.height() == b.height() && a.as_bytes() == b.as_bytes()
}
pub fn decode_image_bytes(
    bytes: &[u8],
) -> Result<Pixels, Box<dyn std::error::Error + Send + Sync>> {
    let rgba = image::load_from_memory(bytes)?.into_rgba8();
    Ok(SharedPixelBuffer::clone_from_slice(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
    ))
}
pub fn downscale_for_display(pixels: &Pixels, max_side: u32) -> Pixels {
    if max_side == 0 || pixels.width().max(pixels.height()) <= max_side {
        return pixels.clone();
    }
    let Some(rgba) =
        image::RgbaImage::from_raw(pixels.width(), pixels.height(), pixels.as_bytes().to_vec())
    else {
        return pixels.clone();
    };
    let resized = image::DynamicImage::ImageRgba8(rgba)
        .resize(max_side, max_side, image::imageops::FilterType::Triangle)
        .into_rgba8();
    SharedPixelBuffer::clone_from_slice(resized.as_raw(), resized.width(), resized.height())
}
#[cfg(target_os = "linux")]
pub fn trim_heap() {
    unsafe {
        libc::malloc_trim(0);
    }
}
#[cfg(not(target_os = "linux"))]
pub fn trim_heap() {}

static ENTRY_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
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
fn url_lock(dir: &Path, url: &str) -> Arc<Mutex<()>> {
    entry_lock(&dir.join(format!("{:016x}.lock", fnv1a(url.as_bytes()))))
}
static CACHE_MUTATION: LazyLock<RwLock<()>> = LazyLock::new(|| RwLock::new(()));
static CACHE_GENERATION: AtomicU64 = AtomicU64::new(0);
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static ENCODINGS: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());
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

struct Processed {
    pixels: Pixels,
    encoded: Option<Vec<u8>>,
}
fn encode_rgba(rgba: &image::RgbaImage, recipe: ImageRecipe) -> Option<Vec<u8>> {
    let _permit = EncodingPermit::acquire();
    let mut bytes = Vec::new();
    match recipe.format {
        Some(CacheImageFormat::Webp) => {
            bytes = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height())
                .encode_simple(false, recipe.quality as f32)
                .ok()?
                .to_vec();
        }
        Some(CacheImageFormat::Jpeg) if rgba.pixels().all(|p| p.0[3] == 255) => {
            let rgb = image::DynamicImage::ImageRgba8(rgba.clone()).into_rgb8();
            image::ImageEncoder::write_image(
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, recipe.quality),
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
            .ok()?;
        }
        _ => {
            image::ImageEncoder::write_image(
                image::codecs::png::PngEncoder::new(&mut bytes),
                rgba.as_raw(),
                rgba.width(),
                rgba.height(),
                image::ExtendedColorType::Rgba8,
            )
            .ok()?;
        }
    }
    Some(bytes)
}
fn process_source(bytes: Vec<u8>, recipe: ImageRecipe) -> Option<Processed> {
    process_source_with(bytes, recipe, encode_rgba)
}
fn process_source_with(
    bytes: Vec<u8>,
    recipe: ImageRecipe,
    encoder: impl FnOnce(&image::RgbaImage, ImageRecipe) -> Option<Vec<u8>>,
) -> Option<Processed> {
    let source = image::load_from_memory(&bytes).ok()?;
    let resized = match recipe.max_side {
        Some(0) => return None,
        Some(side) if source.width().max(source.height()) > side => {
            let fitted = source.resize(side, side, image::imageops::FilterType::Triangle);
            // The full-size decode is only a resize input, never a second
            // retained buffer while encoding or constructing Slint pixels.
            drop(source);
            fitted
        }
        _ => source,
    };
    let rgba = resized.into_rgba8();
    let encoded = if !recipe.compress && recipe.max_side.is_none() {
        Some(bytes)
    } else {
        drop(bytes);
        encoder(&rgba, recipe)
    };
    // Construct only processed-size Slint buffers, and prefer the decoded encoded
    // representation so cold, memory and disk hits have exactly the same quality.
    if let Some(encoded) = encoded
        && let Ok(pixels) = decode_image_bytes(&encoded)
    {
        return Some(Processed {
            pixels,
            encoded: Some(encoded),
        });
    }
    Some(Processed {
        pixels: SharedPixelBuffer::clone_from_slice(rgba.as_raw(), rgba.width(), rgba.height()),
        encoded: None,
    })
}

const MAGIC: &[u8; 8] = b"NVIMAGE2";
#[derive(Clone, Debug, Serialize, Deserialize)]
struct VariantMetadata {
    version: u32,
    url: String,
    uses: Vec<ImageUse>,
    width: u32,
    height: u32,
    recipe: ImageRecipe,
    checksum: u64,
}
struct DiskVariant {
    metadata: VariantMetadata,
    encoded: Vec<u8>,
}
fn recipe_disk_path(dir: &Path, url: &str, recipe: ImageRecipe) -> PathBuf {
    dir.join(format!(
        "{:016x}-{:016x}.variant",
        fnv1a(url.as_bytes()),
        fnv1a(recipe.key().as_bytes())
    ))
}
fn read_variant(path: &Path) -> Option<DiskVariant> {
    let data = fs::read(path).ok()?;
    if data.get(..8)? != MAGIC {
        return None;
    }
    let length = u32::from_le_bytes(data.get(8..12)?.try_into().ok()?) as usize;
    if length > 64 * 1024 {
        return None;
    }
    let end = 12usize.checked_add(length)?;
    let metadata: VariantMetadata = serde_json::from_slice(data.get(12..end)?).ok()?;
    let encoded = data.get(end..)?.to_vec();
    if metadata.version != 2 || metadata.uses.is_empty() || metadata.checksum != fnv1a(&encoded) {
        return None;
    }
    Some(DiskVariant { metadata, encoded })
}
fn variant_pixels(variant: &DiskVariant, url: &str, recipe: ImageRecipe) -> Option<Pixels> {
    if variant.metadata.url != url || variant.metadata.recipe != recipe {
        return None;
    }
    let pixels = decode_image_bytes(&variant.encoded).ok()?;
    if pixels.width() != variant.metadata.width
        || pixels.height() != variant.metadata.height
        || recipe
            .max_side
            .is_some_and(|side| pixels.width().max(pixels.height()) > side)
    {
        return None;
    }
    Some(pixels)
}
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("missing cache filename"))?
        .to_string_lossy();
    let temp = path.with_file_name(format!(
        "{name}.tmp-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
fn write_variant(path: &Path, variant: &DiskVariant) -> std::io::Result<()> {
    let metadata = serde_json::to_vec(&variant.metadata)?;
    let mut data = Vec::with_capacity(12 + metadata.len() + variant.encoded.len());
    data.extend_from_slice(MAGIC);
    data.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    data.extend_from_slice(&metadata);
    data.extend_from_slice(&variant.encoded);
    atomic_write(path, &data)
}
fn variant_paths(dir: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "variant"))
}
fn directory_variants(dir: &Path) -> impl Iterator<Item = (PathBuf, DiskVariant)> {
    // Stream files rather than retaining the entire compressed disk cache while
    // collecting metadata for maintenance.
    variant_paths(dir).filter_map(|p| read_variant(&p).map(|v| (p, v)))
}
/// Move a use only after its replacement succeeded. Other consumers of an old
/// recipe keep that file until their own successful load migrates them.
fn retire_use_history(dir: &Path, url: &str, uses: &[ImageUse], keep: &Path) {
    let prefix = format!("{:016x}", fnv1a(url.as_bytes()));
    // Most loads replace one URL's variants. Do not read every other URL's
    // payload just to retire that URL's older processing settings.
    let history = variant_paths(dir)
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .filter_map(|path| read_variant(&path).map(|variant| (path, variant)));
    for (path, mut variant) in history {
        if path == keep || variant.metadata.url != url {
            continue;
        }
        let previous = variant.metadata.uses.len();
        variant
            .metadata
            .uses
            .retain(|image_use| !uses.contains(image_use));
        if previous == variant.metadata.uses.len() {
            continue;
        }
        if variant.metadata.uses.is_empty() {
            let _ = fs::remove_file(path);
        } else {
            let _ = write_variant(&path, &variant);
        }
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == format!("{prefix}.img")
                || name == format!("{prefix}.cfg")
                || name.starts_with(&format!("{prefix}.d"))
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}
fn persist_processed(
    dir: &Path,
    url: &str,
    uses: &[ImageUse],
    recipe: ImageRecipe,
    processed: &Processed,
) -> std::io::Result<()> {
    let encoded = processed
        .encoded
        .as_ref()
        .ok_or_else(|| std::io::Error::other("image encoding failed"))?;
    fs::create_dir_all(dir)?;
    let path = recipe_disk_path(dir, url, recipe);
    let mut registered = read_variant(&path)
        .filter(|v| v.metadata.url == url && v.metadata.recipe == recipe)
        .map(|v| v.metadata.uses)
        .unwrap_or_default();
    for image_use in uses {
        if !registered.contains(image_use) {
            registered.push(*image_use);
        }
    }
    let variant = DiskVariant {
        metadata: VariantMetadata {
            version: 2,
            url: url.into(),
            uses: registered,
            width: processed.pixels.width(),
            height: processed.pixels.height(),
            recipe,
            checksum: fnv1a(encoded),
        },
        encoded: encoded.clone(),
    };
    write_variant(&path, &variant)?;
    retire_use_history(dir, url, uses, &path);
    Ok(())
}
fn register_cached_use(dir: &Path, url: &str, image_use: ImageUse, recipe: ImageRecipe) -> bool {
    let lock = url_lock(dir, url);
    let _guard = lock.lock().unwrap();
    let path = recipe_disk_path(dir, url, recipe);
    let Some(mut variant) =
        read_variant(&path).filter(|v| v.metadata.url == url && v.metadata.recipe == recipe)
    else {
        return false;
    };
    if !variant.metadata.uses.contains(&image_use) {
        variant.metadata.uses.push(image_use);
        if write_variant(&path, &variant).is_ok() {
            retire_use_history(dir, url, &[image_use], &path);
        }
    }
    true
}

fn register_refreshed_use(
    dir: &Path,
    url: &str,
    image_use: ImageUse,
    recipe: ImageRecipe,
    pixels: &Pixels,
) {
    let lock = url_lock(dir, url);
    let _guard = lock.lock().unwrap();
    let path = recipe_disk_path(dir, url, recipe);
    let Some(mut variant) =
        read_variant(&path).filter(|v| v.metadata.url == url && v.metadata.recipe == recipe)
    else {
        return;
    };
    // A failed atomic replacement leaves the previous valid variant in place.
    // Do not attach a new caller role to that older generation.
    if !variant_pixels(&variant, url, recipe)
        .is_some_and(|stored| image_buffers_equal(&stored, pixels))
    {
        return;
    }
    if !variant.metadata.uses.contains(&image_use) {
        variant.metadata.uses.push(image_use);
        if write_variant(&path, &variant).is_ok() {
            retire_use_history(dir, url, &[image_use], &path);
        }
    }
}

#[derive(Clone, Copy)]
struct LoadEpoch {
    clear: u64,
    settings: u64,
}
impl LoadEpoch {
    fn capture() -> Self {
        Self {
            clear: CACHE_GENERATION.load(Ordering::Acquire),
            settings: image_settings_revision(),
        }
    }
    fn current(self) -> bool {
        self.clear == CACHE_GENERATION.load(Ordering::Acquire)
            && self.settings == image_settings_revision()
    }
}
type Flight = Arc<(Mutex<Option<Option<Pixels>>>, Condvar)>;
static IN_FLIGHT: LazyLock<Mutex<HashMap<String, Flight>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
// Refreshes must always consult the transport, so they share completion slots
// with other refreshes but never join ordinary cache loads.
static REFRESH_IN_FLIGHT: LazyLock<Mutex<HashMap<String, Flight>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// All transports call this core. The injected source fetch makes behavior tests
/// independent of HTTP. Neither old variants nor originals can satisfy a new recipe.
pub fn cached_pixels_with(
    dir: &Path,
    url: &str,
    image_use: impl Into<ImageUse>,
    settings: &CacheSettings,
    download: impl FnOnce() -> Option<Vec<u8>>,
) -> Option<Pixels> {
    cached_pixels_at(
        dir,
        url,
        image_use.into(),
        settings,
        LoadEpoch::capture(),
        download,
    )
}
fn cached_pixels_at(
    dir: &Path,
    url: &str,
    image_use: ImageUse,
    settings: &CacheSettings,
    epoch: LoadEpoch,
    download: impl FnOnce() -> Option<Vec<u8>>,
) -> Option<Pixels> {
    let recipe = ImageRecipe::resolve(image_use, settings);
    let key = image_recipe_cache_key(url, recipe);
    if let Some(pixels) = decoded_cache_get(&key) {
        if !settings.cache_images {
            return epoch.current().then_some(pixels);
        }
        let _mutation = CACHE_MUTATION.read().unwrap();
        if !epoch.current() {
            return None;
        }
        if register_cached_use(dir, url, image_use, recipe) {
            return Some(pixels);
        }
        // A memory entry from a disk-disabled load has no source bytes to save.
        // Fetch its source rather than recompressing the decoded lossy image.
    }
    let flight_key = format!(
        "{}:{}:{}:{}",
        dir.display(),
        epoch.clear,
        epoch.settings,
        key
    );
    let (flight, leader) = {
        let mut flights = IN_FLIGHT.lock().unwrap();
        if let Some(flight) = flights.get(&flight_key) {
            (flight.clone(), false)
        } else {
            let flight = Arc::new((Mutex::new(None), Condvar::new()));
            flights.insert(flight_key.clone(), flight.clone());
            (flight, true)
        }
    };
    if !leader {
        let mut result = flight.0.lock().unwrap();
        while result.is_none() {
            result = flight.1.wait(result).unwrap();
        }
        let pixels = result.as_ref().unwrap().clone();
        drop(result);
        if pixels.is_some() && settings.cache_images {
            let _mutation = CACHE_MUTATION.read().unwrap();
            if epoch.current() {
                register_cached_use(dir, url, image_use, recipe);
            }
        }
        return if epoch.current() { pixels } else { None };
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        load_recipe(dir, url, image_use, settings, &key, epoch, download)
    }))
    .ok()
    .flatten();
    {
        let mut flights = IN_FLIGHT.lock().unwrap();
        *flight.0.lock().unwrap() = Some(result.clone());
        flights.remove(&flight_key);
        flight.1.notify_all();
    }
    result
}
fn load_recipe(
    dir: &Path,
    url: &str,
    image_use: ImageUse,
    settings: &CacheSettings,
    key: &str,
    epoch: LoadEpoch,
    download: impl FnOnce() -> Option<Vec<u8>>,
) -> Option<Pixels> {
    let recipe = ImageRecipe::resolve(image_use, settings);
    let disk = settings.cache_images;
    if disk {
        let _mutation = CACHE_MUTATION.read().unwrap();
        let lock = url_lock(dir, url);
        let _guard = lock.lock().unwrap();
        if !epoch.current() {
            return None;
        }
        let path = recipe_disk_path(dir, url, recipe);
        if let Some(mut variant) = read_variant(&path)
            && let Some(pixels) = variant_pixels(&variant, url, recipe)
        {
            if !variant.metadata.uses.contains(&image_use) {
                variant.metadata.uses.push(image_use);
                if write_variant(&path, &variant).is_ok() {
                    retire_use_history(dir, url, &[image_use], &path);
                }
            }
            decoded_cache_insert(key, pixels.clone());
            return Some(pixels);
        }
    }
    let processed = process_source(download()?, recipe)?;
    let _mutation = CACHE_MUTATION.read().unwrap();
    if !epoch.current() {
        return None;
    }
    if processed.encoded.is_some() {
        if disk {
            let lock = url_lock(dir, url);
            let _guard = lock.lock().unwrap();
            let _ = persist_processed(dir, url, &[image_use], recipe, &processed);
        }
        decoded_cache_insert(key, processed.pixels.clone());
    }
    Some(processed.pixels)
}
pub fn display_pixels(url: &str, image_use: impl Into<ImageUse>) -> Option<Pixels> {
    let clear = CACHE_GENERATION.load(Ordering::Acquire);
    let (settings, revision) = nova_config::image_settings_snapshot();
    cached_pixels_at(
        &poster_cache_dir(),
        url,
        image_use.into(),
        &settings,
        LoadEpoch {
            clear,
            settings: revision,
        },
        || crate::net::blocking_image_bytes(url),
    )
}

/// Refresh the exact same processing recipe. Comparison happens after encoding;
/// unchanged lossy pixels do not cause a visible swap on every refresh.
pub fn refresh_image_if_changed(
    url: String,
    image_use: impl Into<ImageUse>,
    then: impl FnOnce(Option<Pixels>) + Send + 'static,
) {
    let image_use = image_use.into();
    let clear = CACHE_GENERATION.load(Ordering::Acquire);
    let (settings, revision) = nova_config::image_settings_snapshot();
    let epoch = LoadEpoch {
        clear,
        settings: revision,
    };
    std::thread::Builder::new()
        .name("image-refresh".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                refresh_image_if_changed_with(
                    &poster_cache_dir(),
                    &url,
                    image_use,
                    &settings,
                    epoch,
                    || crate::net::blocking_image_bytes(&url),
                )
            }))
            .ok()
            .flatten();
            then(result);
        })
        .expect("spawn image refresh worker");
}

fn refresh_image_if_changed_with(
    dir: &Path,
    url: &str,
    image_use: ImageUse,
    settings: &CacheSettings,
    epoch: LoadEpoch,
    download: impl FnOnce() -> Option<Vec<u8>>,
) -> Option<Pixels> {
    let recipe = ImageRecipe::resolve(image_use, settings);
    let key = image_recipe_cache_key(url, recipe);
    let previous = decoded_cache_get(&key);
    let flight_key = format!(
        "{}:{}:{}:{}",
        dir.display(),
        epoch.clear,
        epoch.settings,
        key
    );
    let (flight, leader) = {
        let mut flights = REFRESH_IN_FLIGHT.lock().unwrap();
        if let Some(flight) = flights.get(&flight_key) {
            (flight.clone(), false)
        } else {
            let flight = Arc::new((Mutex::new(None), Condvar::new()));
            flights.insert(flight_key.clone(), flight.clone());
            (flight, true)
        }
    };

    let refreshed = if leader {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let processed = process_source(download()?, recipe)?;
            let _mutation = CACHE_MUTATION.read().unwrap();
            if !epoch.current() {
                return None;
            }
            if processed.encoded.is_some() {
                if settings.cache_images {
                    let lock = url_lock(dir, url);
                    let _guard = lock.lock().unwrap();
                    let _ = persist_processed(dir, url, &[image_use], recipe, &processed);
                }
                decoded_cache_insert(&key, processed.pixels.clone());
            }
            Some(processed.pixels)
        }))
        .ok()
        .flatten();
        {
            let mut flights = REFRESH_IN_FLIGHT.lock().unwrap();
            *flight.0.lock().unwrap() = Some(result.clone());
            flights.remove(&flight_key);
            flight.1.notify_all();
        }
        result
    } else {
        let mut result = flight.0.lock().unwrap();
        while result.is_none() {
            result = flight.1.wait(result).unwrap();
        }
        result.as_ref().unwrap().clone()
    };

    let refreshed = refreshed?;
    if !epoch.current() {
        return None;
    }
    // A refresh may be requested by another semantic use that resolves to the
    // same recipe. Record each caller's use after the shared generation ends.
    if settings.cache_images {
        let _mutation = CACHE_MUTATION.read().unwrap();
        if !epoch.current() {
            return None;
        }
        register_refreshed_use(dir, url, image_use, recipe, &refreshed);
    }
    previous
        .as_ref()
        .is_none_or(|old| !image_buffers_equal(old, &refreshed))
        .then_some(refreshed)
}

static MAINTENANCE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
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
        rewrite_entries_with(
            dir,
            settings,
            cancel,
            progress,
            crate::net::blocking_image_bytes,
        )
    }
}
impl Drop for MaintenanceGuard {
    fn drop(&mut self) {
        *MAINTENANCE.0.lock().unwrap() = false;
        MAINTENANCE.1.notify_all();
    }
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
fn rewrite_entries_with(
    dir: &Path,
    settings: &CacheSettings,
    cancel: &AtomicBool,
    mut progress: impl FnMut(RewriteProgress),
    mut download: impl FnMut(&str) -> Option<Vec<u8>>,
) -> RewriteProgress {
    let mut result = RewriteProgress::default();
    let mut urls: HashMap<String, HashSet<ImageUse>> = HashMap::new();
    for (_, variant) in directory_variants(dir) {
        urls.entry(variant.metadata.url)
            .or_default()
            .extend(variant.metadata.uses);
    }
    result.total = urls.len();
    progress(result);
    // A snapshot deliberately remains valid if settings are changed by another
    // device during maintenance. Ordinary loads resolve their newer recipes.
    let generation = CACHE_GENERATION.load(Ordering::Acquire);
    for (url, uses) in urls {
        if cancel.load(Ordering::Acquire) {
            result.cancelled = true;
            break;
        }
        if !settings.cache_images {
            result.skipped += 1;
            result.processed += 1;
            progress(result);
            continue;
        }
        let Some(bytes) = download(&url) else {
            result.failed += 1;
            result.processed += 1;
            progress(result);
            continue;
        };
        // Decode the source once per URL; no recipe starts from a cached derivative.
        let Ok(source) = image::load_from_memory(&bytes) else {
            result.failed += 1;
            result.processed += 1;
            progress(result);
            continue;
        };
        let mut recipes: HashMap<String, (ImageRecipe, Vec<ImageUse>)> = HashMap::new();
        for image_use in uses {
            let recipe = ImageRecipe::resolve(image_use, settings);
            recipes
                .entry(recipe.key())
                .or_insert_with(|| (recipe, Vec::new()))
                .1
                .push(image_use);
        }
        for (_, (recipe, uses)) in recipes {
            if cancel.load(Ordering::Acquire) {
                result.cancelled = true;
                break;
            }
            let fitted = match recipe.max_side {
                Some(side) if source.width().max(source.height()) > side => {
                    source.resize(side, side, image::imageops::FilterType::Triangle)
                }
                _ => source.clone(),
            };
            let rgba = fitted.into_rgba8();
            let encoded = if !recipe.compress && recipe.max_side.is_none() {
                Some(bytes.clone())
            } else {
                encode_rgba(&rgba, recipe)
            };
            let Some(encoded) = encoded else {
                result.failed += 1;
                continue;
            };
            let Ok(pixels) = decode_image_bytes(&encoded) else {
                result.failed += 1;
                continue;
            };
            let processed = Processed {
                pixels,
                encoded: Some(encoded),
            };
            let _mutation = CACHE_MUTATION.read().unwrap();
            if CACHE_GENERATION.load(Ordering::Acquire) != generation {
                result.cancelled = true;
                break;
            }
            let lock = url_lock(dir, &url);
            let _guard = lock.lock().unwrap();
            if persist_processed(dir, &url, &uses, recipe, &processed).is_ok() {
                decoded_cache_insert(&image_recipe_cache_key(&url, recipe), processed.pixels);
                result.converted += 1;
            } else {
                result.failed += 1;
            }
        }
        result.processed += 1;
        progress(result);
        if result.cancelled {
            break;
        }
    }
    progress(result);
    result
}
pub fn rewrite_cache_dir_with_progress(
    dir: &Path,
    settings: &CacheSettings,
    cancel: &AtomicBool,
    progress: impl FnMut(RewriteProgress),
) -> RewriteProgress {
    MaintenanceGuard::acquire().rewrite(dir, settings, cancel, progress)
}
pub fn rewrite_cache_dir_to_format(dir: &Path, settings: &CacheSettings) -> usize {
    rewrite_cache_dir_with_progress(dir, settings, &AtomicBool::new(false), |_| {}).converted
}
pub fn clear_poster_cache_dir(dir: &Path) -> (u64, usize) {
    MaintenanceGuard::acquire().clear(dir)
}
fn clear_cache_entries(dir: &Path) -> (u64, usize) {
    let _mutation = CACHE_MUTATION.write().unwrap();
    CACHE_GENERATION.fetch_add(1, Ordering::AcqRel);
    decoded_cache_clear();
    let mut total = (0u64, 0);
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(metadata) = entry.metadata()
                && metadata.is_file()
                && fs::remove_file(path).is_ok()
            {
                total.0 = total.0.saturating_add(metadata.len());
                total.1 += 1;
            }
        }
    }
    total
}
pub fn poster_cache_disk_usage(dir: &Path) -> (u64, usize) {
    fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .fold((0u64, 0), |(bytes, count), metadata| {
            (bytes.saturating_add(metadata.len()), count + 1)
        })
}

/// Compatibility byte helpers use processed Original variants, never a backup.
pub fn poster_cache_path_in(dir: &Path, url: &str) -> PathBuf {
    recipe_disk_path(
        dir,
        url,
        ImageRecipe::resolve(ImageUse::Fixed(None), &active_cache_settings()),
    )
}
pub fn read_poster_bytes(dir: &Path, url: &str) -> Option<Vec<u8>> {
    let recipe = ImageRecipe::resolve(ImageUse::Fixed(None), &active_cache_settings());
    let _mutation = CACHE_MUTATION.read().unwrap();
    let lock = url_lock(dir, url);
    let _guard = lock.lock().unwrap();
    read_variant(&recipe_disk_path(dir, url, recipe))
        .filter(|v| v.metadata.url == url && v.metadata.recipe == recipe)
        .map(|v| v.encoded)
}
pub fn write_poster_bytes(dir: &Path, url: &str, bytes: &[u8]) {
    let recipe = ImageRecipe::resolve(ImageUse::Fixed(None), &active_cache_settings());
    if let Some(processed) = process_source(bytes.to_vec(), recipe) {
        let _mutation = CACHE_MUTATION.read().unwrap();
        let lock = url_lock(dir, url);
        let _guard = lock.lock().unwrap();
        let _ = persist_processed(dir, url, &[ImageUse::Fixed(None)], recipe, &processed);
    }
}
#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;

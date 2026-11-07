//! Nova's transport and profile adapters for the shared image cache.
//!
//! Recipes, pixels, disk variants, maintenance and eviction live in nova-kit.
//! HTTP remains in `net`; the full synced profile remains in `nova-config`.
use nova_config::CacheSettings;
use slint::{Rgba8Pixel, SharedPixelBuffer};
use std::{
    path::Path,
    sync::{LazyLock, atomic::AtomicBool},
};

pub use nova_cache::{
    DETAIL_BACKDROP_SIDE, DISPLAY_POSTER_SIDE, EPISODE_THUMB_SIDE, ImageRecipe, ImageUse,
    RewriteProgress, UNLOAD_EXTRA_CARDS, backdrop_pixels_cached, clear_poster_cache_dir,
    decode_image_bytes, decoded_cache_clear, decoded_cache_contains, decoded_cache_get,
    decoded_cache_insert, decoded_cache_stats, downscale_for_display, image_buffers_equal,
    image_cache_key, poster_cache_disk_usage, poster_cache_path_in, read_poster_bytes,
    set_decoded_cache_budget, sized_cache_key, trim_heap, write_poster_bytes,
};

type Pixels = SharedPixelBuffer<Rgba8Pixel>;

fn configure() {
    static CONFIGURED: LazyLock<()> = LazyLock::new(|| {
        nova_cache::config::configure(
            nova_config::poster_cache_dir(),
            crate::net::blocking_image_bytes,
        );
    });
    LazyLock::force(&CONFIGURED);
}

pub fn cached_pixels_with(
    dir: &Path,
    url: &str,
    image_use: impl Into<ImageUse>,
    settings: &CacheSettings,
    download: impl FnOnce() -> Option<Vec<u8>>,
) -> Option<Pixels> {
    nova_cache::cached_pixels_with(
        dir,
        url,
        image_use,
        &settings.image_cache_settings(),
        download,
    )
}

pub fn cached_display_pixels(url: &str, image_use: impl Into<ImageUse>) -> Option<Pixels> {
    configure();
    nova_cache::cached_display_pixels(url, image_use)
}

pub fn display_pixels(url: &str, image_use: impl Into<ImageUse>) -> Option<Pixels> {
    configure();
    nova_cache::display_pixels(url, image_use)
}

pub fn refresh_image_if_changed(
    url: String,
    image_use: impl Into<ImageUse>,
    then: impl FnOnce(Option<Pixels>) + Send + 'static,
) {
    configure();
    nova_cache::refresh_image_if_changed(url, image_use, then);
}

/// Retain the app-facing settings type while sharing maintenance ownership.
pub struct MaintenanceGuard(nova_cache::MaintenanceGuard);
impl MaintenanceGuard {
    pub fn try_acquire() -> Option<Self> {
        configure();
        nova_cache::MaintenanceGuard::try_acquire().map(Self)
    }

    pub fn clear(&self, dir: &Path) -> (u64, usize) {
        self.0.clear(dir)
    }

    pub fn rewrite(
        &self,
        dir: &Path,
        settings: &CacheSettings,
        cancel: &AtomicBool,
        progress: impl FnMut(RewriteProgress),
    ) -> RewriteProgress {
        self.0
            .rewrite(dir, &settings.image_cache_settings(), cancel, progress)
    }
}

pub fn rewrite_cache_dir_with_progress(
    dir: &Path,
    settings: &CacheSettings,
    cancel: &AtomicBool,
    progress: impl FnMut(RewriteProgress),
) -> RewriteProgress {
    configure();
    nova_cache::rewrite_cache_dir_with_progress(
        dir,
        &settings.image_cache_settings(),
        cancel,
        progress,
    )
}

pub fn rewrite_cache_dir_to_format(dir: &Path, settings: &CacheSettings) -> usize {
    configure();
    nova_cache::rewrite_cache_dir_to_format(dir, &settings.image_cache_settings())
}

pub mod gc {
    pub use nova_cache::gc::{
        BATCH_INTERVAL, Backend, Marker, ROUTINE_INTERVAL_SECS, Report, Worker, cleanup_due, wake,
    };
    use std::{io, path::PathBuf, sync::Arc};

    pub fn start(directory: PathBuf, backend: Arc<dyn Backend>) -> io::Result<Worker> {
        super::configure();
        nova_cache::gc::start(directory, backend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_existing_nova_variant_without_refetching_or_rewriting() {
        // Written by the unmodified Nova b69a1e5 cache.rs/cache_gc.rs, before
        // extraction. Keep the actual filename and envelope as a migration
        // fixture so recipe/key changes cannot silently abandon existing data.
        const NAME: &str = "80321db8c5745345-9962d87be6b3f06b.variant";
        let bytes = include_bytes!("fixtures/80321db8c5745345-9962d87be6b3f06b.variant");
        let dir = std::env::temp_dir().join(format!(
            "nova-cache-migration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(NAME);
        std::fs::write(&path, bytes).unwrap();
        let settings = CacheSettings {
            enabled: false,
            ..Default::default()
        };
        let pixels = cached_pixels_with(
            &dir,
            "https://example.invalid/nova-existing-poster.png",
            ImageUse::DetailPoster,
            &settings,
            || panic!("existing Nova variant must satisfy the shared cache"),
        )
        .unwrap();
        assert_eq!((pixels.width(), pixels.height()), (2, 3));
        assert!(
            pixels
                .as_bytes()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == [17, 83, 149, 255])
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

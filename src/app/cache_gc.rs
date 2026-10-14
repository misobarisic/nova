//! App-owned local maintenance markers and Library retention policy.
use super::*;
use nova_media::cache::gc::{self, Backend, Report, Worker};
use std::{io, path::Path};

#[derive(Serialize, Deserialize)]
struct Marker {
    directory: PathBuf,
    completed: u64,
}

pub(super) fn last_cleanup(key: &str, directory: &Path) -> io::Result<Option<u64>> {
    let raw = storage::try_get_str(key).map_err(io::Error::other)?;
    // These are disposable local scheduling hints, not authoritative data.
    // A missing, corrupt or moved-cache marker simply makes cleanup due.
    Ok(raw
        .and_then(|raw| serde_json::from_str::<Marker>(&raw).ok())
        .filter(|marker| marker.directory == directory)
        .map(|marker| marker.completed))
}

pub(super) fn save_cleanup(key: &str, directory: &Path, completed: u64) -> io::Result<()> {
    let marker = Marker {
        directory: directory.to_owned(),
        completed,
    };
    storage::try_set_str(key, &serde_json::to_string(&marker)?).map_err(io::Error::other)
}

struct ImageBackend(Bridge);
impl Backend for ImageBackend {
    fn last_cleanup(&self, directory: &Path) -> io::Result<Option<u64>> {
        last_cleanup("cache_gc:image:v1", directory)
    }
    fn save_cleanup(&self, directory: &Path, completed: u64) -> io::Result<()> {
        save_cleanup("cache_gc:image:v1", directory, completed)
    }
    fn preferred_urls(&self) -> io::Result<HashSet<String>> {
        // Borrow title identities under the shared lock, then read one header
        // and episode cache at a time outside it. Never retain decoded pixels.
        let titles = self
            .0
            .shared
            .lock()
            .unwrap()
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.id.clone(),
                    entry.type_.clone(),
                    entry.poster_url.clone(),
                    entry.background_url.clone(),
                )
            })
            .collect::<Vec<_>>();
        let mut urls = HashSet::new();
        for (id, type_, poster, background) in titles {
            urls.extend(
                [poster, background]
                    .into_iter()
                    .filter(|url| !url.is_empty()),
            );
            if let Some(header) = read_meta_header_for(&type_, &id) {
                urls.extend(
                    [header.poster_url, header.background_url, header.logo_url]
                        .into_iter()
                        .filter(|url| !url.is_empty()),
                );
                urls.extend(
                    header
                        .season_backdrops
                        .into_values()
                        .filter(|url| !url.is_empty()),
                );
            }
            if let Some(videos) = read_episodes_cache_for(&type_, &id) {
                urls.extend(
                    videos
                        .into_iter()
                        .filter_map(|video| video.thumbnail)
                        .filter(|url| !url.is_empty()),
                );
            }
        }
        Ok(urls)
    }
    fn completed(&self, report: Report) {
        tracing::info!(
            files = report.removed_files,
            bytes = report.removed_bytes,
            remaining_bytes = report.remaining_bytes,
            "image cache cleanup completed"
        );
        let bridge = self.0.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if bridge.app().is_some_and(|app| app.get_show_settings()) {
                bridge.refresh_cache_disk_usage();
            }
        });
    }
    fn failed(&self, error: &io::Error) {
        tracing::warn!(%error, "image cache maintenance failed");
    }
}

pub(super) fn start(bridge: &Bridge) -> Option<Worker> {
    match gc::start(poster_cache_dir(), Arc::new(ImageBackend(bridge.clone()))) {
        Ok(worker) => Some(worker),
        Err(error) => {
            tracing::warn!(%error, "could not start image cache maintenance");
            None
        }
    }
}

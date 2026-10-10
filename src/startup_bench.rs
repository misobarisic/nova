//! Opt-in measurements of the real desktop startup, never persisted in app state.
//!
//! Rendering callbacks only collect timestamps. Result IO and optional validation
//! snapshots happen after the observation window, outside the measured render.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use slint::{ComponentHandle, Model, RenderingState};

use crate::AppWindow;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
static RECORDER: OnceLock<Arc<Recorder>> = OnceLock::new();
thread_local! {
    static STACK: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Navigation {
    Home,
    Library,
    Settings,
}

#[derive(Deserialize)]
struct Options {
    output: PathBuf,
    navigation: Navigation,
    #[serde(default)]
    validate_snapshot: bool,
}

#[derive(Default, Serialize)]
struct Measurements {
    milestones_us: BTreeMap<&'static str, u64>,
    spans: Vec<CompletedSpan>,
    heartbeat_lateness_us: Vec<u64>,
    home_counts: BTreeMap<&'static str, usize>,
    window_width: u32,
    window_height: u32,
    scale_factor: f32,
    storage_failed: bool,
    models_ready: bool,
    navigation_started: bool,
    finished: bool,
    truncated_spans: bool,
}

impl Measurements {
    fn mark(&mut self, name: &'static str, at: u64) {
        // Renderer setup may recur after minimization or window recreation.
        // Keep the first observation, never rewrite startup with a later frame.
        self.milestones_us.entry(name).or_insert(at);
    }

    fn rendered(&mut self, at: u64, featured_art: bool, navigation: Option<Navigation>) {
        self.mark("first_render", at);
        if self.models_ready {
            self.mark("home_ready_render", at);
        }
        if featured_art {
            self.mark("featured_art_render", at);
        }
        if self.navigation_started && navigation.is_some() {
            self.mark("navigation_render", at);
        }
    }
}

struct Recorder {
    started: Instant,
    options: Options,
    measurements: Mutex<Measurements>,
}

impl Recorder {
    fn elapsed_us(&self) -> u64 {
        self.started.elapsed().as_micros() as u64
    }
}

#[derive(Serialize)]
struct CompletedSpan {
    id: u64,
    parent_id: Option<u64>,
    name: &'static str,
    start_us: u64,
    duration_us: u64,
}

/// Initialize before app startup. Returns true for the fixture-only command,
/// which intentionally exits without opening a Slint window.
pub fn initialize(started: Instant) -> Result<bool> {
    if let Some(path) = std::env::var_os("NOVA_STARTUP_BENCH_PREPARE") {
        prepare(&PathBuf::from(path))?;
        return Ok(true);
    }
    if std::env::var("NOVA_STARTUP_BENCH").as_deref() != Ok("1") {
        return Ok(false);
    }
    let path = std::env::var_os("NOVA_STARTUP_BENCH_CONFIG")
        .ok_or("NOVA_STARTUP_BENCH_CONFIG is required")?;
    let options: Options = serde_json::from_slice(&std::fs::read(path)?)?;
    RECORDER
        .set(Arc::new(Recorder {
            started,
            options,
            measurements: Mutex::new(Measurements::default()),
        }))
        .map_err(|_| "startup recorder already initialized")?;
    Ok(false)
}

/// A nested phase guard. Disabled measurements allocate nothing.
pub struct Span {
    active: Option<ActiveSpan>,
}

struct ActiveSpan {
    recorder: Arc<Recorder>,
    id: u64,
    parent_id: Option<u64>,
    name: &'static str,
    start_us: u64,
}

pub fn span(name: &'static str) -> Span {
    let active = RECORDER.get().map(|recorder| {
        let start = recorder.elapsed_us();
        let (id, parent) = STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            let parent = stack.last().copied();
            // A process-wide sequence avoids collisions if a future phase
            // moves onto a worker. Parentage remains thread-local.
            static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            stack.push(id);
            (id, parent)
        });
        ActiveSpan {
            recorder: recorder.clone(),
            id,
            parent_id: parent,
            name,
            start_us: start,
        }
    });
    Span { active }
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some(ActiveSpan {
            recorder,
            id,
            parent_id,
            name,
            start_us,
        }) = self.active.take()
        else {
            return;
        };
        let duration_us = recorder.elapsed_us().saturating_sub(start_us);
        STACK.with(|stack| {
            let popped = stack.borrow_mut().pop();
            debug_assert_eq!(popped, Some(id));
        });
        let mut data = recorder.measurements.lock().unwrap();
        if !data.finished {
            if data.spans.len() < 4096 {
                data.spans.push(CompletedSpan {
                    id,
                    parent_id,
                    name,
                    start_us,
                    duration_us,
                });
            } else {
                data.truncated_spans = true;
            }
        }
    }
}

pub(crate) fn player(
    app: &AppWindow,
    mut observer: impl FnMut(RenderingState) + 'static,
) -> crate::player::Player {
    let Some(recorder) = RECORDER.get() else {
        return crate::player::Player::setup_with_render_observer(app, observer);
    };
    let recorder = recorder.clone();
    let weak = app.as_weak();
    crate::player::Player::setup_with_render_observer(app, move |state| {
        observer(state.clone());
        let at = recorder.elapsed_us();
        let mut data = recorder.measurements.lock().unwrap();
        if data.finished {
            return;
        }
        match state {
            RenderingState::RenderingSetup => data.mark("renderer_setup", at),
            RenderingState::BeforeRendering => data.mark("first_render_start", at),
            RenderingState::AfterRendering => {
                let Some(app) = weak.upgrade() else { return };
                let navigation = match recorder.options.navigation {
                    Navigation::Library if app.get_show_library() => Some(Navigation::Library),
                    Navigation::Settings if app.get_show_settings() => Some(Navigation::Settings),
                    _ => None,
                };
                data.rendered(
                    at,
                    app.get_home_featured_count() > 0
                        && app.get_home_featured_backdrop().size().width > 0,
                    navigation,
                );
            }
            _ => {}
        }
    })
}

/// Keep this marker with completion of local restore/projection if startup is
/// later deferred. A loading shell must not qualify as a ready Home frame.
pub(crate) fn models_ready(app: &AppWindow) {
    if let Some(recorder) = RECORDER.get() {
        let mut data = recorder.measurements.lock().unwrap();
        data.models_ready = true;
        data.mark("home_models_ready", recorder.elapsed_us());
        data.home_counts = BTreeMap::from([
            ("continue", app.get_home_continue().row_count()),
            ("new_episodes", app.get_home_new_episodes().row_count()),
            ("upcoming", app.get_home_upcoming().row_count()),
            ("catalog_cards", app.get_home_catalog_cards().row_count()),
            ("featured", app.get_home_featured_count().max(0) as usize),
            ("library", app.get_library().row_count()),
            ("addons", app.get_addon_rows().row_count()),
            ("featured_catalogs", app.get_home_catalog_rows().row_count()),
            ("catalog_rows", app.get_home_row_catalog_rows().row_count()),
        ]);
    }
}

/// The caller retains the timer until the real event loop exits.
pub(crate) fn observe(app: &AppWindow) -> Option<slint::Timer> {
    let recorder = RECORDER.get()?.clone();
    app.window().set_size(slint::PhysicalSize::new(1280, 800));
    {
        let mut data = recorder.measurements.lock().unwrap();
        data.mark("event_loop", recorder.elapsed_us());
    }
    let timer = slint::Timer::default();
    let weak = app.as_weak();
    let interval = Duration::from_millis(16);
    let mut deadline = Instant::now() + interval;
    timer.start(slint::TimerMode::Repeated, interval, move || {
        let now = Instant::now();
        let lateness = now.saturating_duration_since(deadline).as_micros() as u64;
        deadline = now + interval;
        let Some(app) = weak.upgrade() else { return };
        let mut data = recorder.measurements.lock().unwrap();
        if data.finished {
            return;
        }
        let Some(first) = data.milestones_us.get("first_render").copied() else {
            return;
        };
        let observing = recorder.elapsed_us().saturating_sub(first) < 3_000_000;
        // Include the heartbeat that completes the observation window. If a
        // stall crosses its boundary, dropping this sample hides the stall.
        if !data.navigation_started {
            data.heartbeat_lateness_us.push(lateness);
        }
        if observing {
            return;
        }
        if !data.milestones_us.contains_key("home_ready_render") {
            return;
        }
        if recorder.options.navigation != Navigation::Home && !data.navigation_started {
            data.navigation_started = true;
            data.mark("navigation_dispatch", recorder.elapsed_us());
            drop(data);
            match recorder.options.navigation {
                Navigation::Library => app.invoke_library_picked(),
                Navigation::Settings => app.invoke_settings_picked(),
                Navigation::Home => unreachable!(),
            }
            // Programmatic navigation has no accompanying pointer event to
            // request a frame; explicitly ask the real backend to render it.
            app.window().request_redraw();
            return;
        }
        if recorder.options.navigation != Navigation::Home
            && !data.milestones_us.contains_key("navigation_render")
        {
            return;
        }
        data.window_width = app.window().size().width;
        data.window_height = app.window().size().height;
        data.scale_factor = app.window().scale_factor();
        data.storage_failed = crate::storage::last_error().is_some();
        data.finished = true;
        drop(data);
        let result = finish(&recorder, &app);
        if let Err(error) = result {
            eprintln!("startup benchmark result failed: {error}");
        }
        let _ = slint::quit_event_loop();
    });
    Some(timer)
}

fn finish(recorder: &Recorder, app: &AppWindow) -> Result<()> {
    if recorder.options.validate_snapshot {
        let snapshot = app.window().take_snapshot()?;
        let first = snapshot
            .as_slice()
            .first()
            .ok_or("empty validation snapshot")?;
        if !snapshot.as_slice().iter().any(|pixel| pixel != first) {
            return Err("validation snapshot contains only a uniform surface".into());
        }
    }
    let data = recorder.measurements.lock().unwrap();
    let output = serde_json::json!({
        "schema_version": 1,
        "navigation": recorder.options.navigation,
        "measurements": *data,
        "snapshot_validated": recorder.options.validate_snapshot,
    });
    let temporary = recorder.options.output.with_extension("tmp");
    std::fs::write(&temporary, serde_json::to_vec(&output)?)?;
    std::fs::rename(temporary, &recorder.options.output)?;
    Ok(())
}

#[derive(Deserialize)]
struct Prepare {
    data_dir: PathBuf,
    #[serde(default)]
    values: BTreeMap<String, Value>,
    #[serde(default)]
    rebase_from: Option<PathBuf>,
    #[serde(default)]
    rebase_cache_from: Option<PathBuf>,
}

fn prepare(path: &Path) -> Result<()> {
    let config: Prepare = serde_json::from_slice(&std::fs::read(path)?)?;
    if config.data_dir != nova_config::app_data_dir() {
        return Err("fixture data directory does not match isolated XDG_DATA_HOME".into());
    }
    crate::storage::init_at(&config.data_dir);
    for (key, mut value) in config.values {
        if key == "settings" {
            let mut defaults = serde_json::to_value(nova_config::CacheSettings::default())?;
            defaults
                .as_object_mut()
                .ok_or("invalid default settings")?
                .extend(
                    value
                        .as_object()
                        .ok_or("fixture settings must be an object")?
                        .clone(),
                );
            value = defaults;
        }
        let raw = serde_json::to_string(&value)?;
        if crate::app::metadata_cache_key(&key) {
            crate::storage::try_set_cached_str(&key, &raw)?;
        } else {
            crate::storage::try_set_str(&key, &raw)?;
        }
    }
    // Relocation changes only device-local filesystem references. Never perform
    // a sync mutation or rewrite cached addon/artwork URLs during preparation.
    if let Some(raw) = crate::storage::try_get_str("downloads:v1")? {
        let mut manifest = crate::download::DownloadManifest::from_json(&raw)?;
        for (index, job) in manifest.jobs.iter_mut().enumerate() {
            if let Some(source) = job.artifact_path.as_ref() {
                let target = relocated_artifact(
                    source,
                    config.rebase_from.as_deref(),
                    &config.data_dir,
                    index,
                );
                if source != &target
                    && let Ok(metadata) = source.metadata()
                    && metadata.is_file()
                {
                    std::fs::create_dir_all(target.parent().ok_or("missing artifact parent")?)?;
                    // Startup checks paths and metadata, not media contents.
                    // Sparse replicas avoid copying multi-gigabyte downloads.
                    std::fs::File::create(&target)?.set_len(metadata.len())?;
                }
                job.artifact_path = Some(target);
            }
        }
        crate::storage::try_set_str("downloads:v1", &manifest.to_json()?)?;
    }
    let cache = nova_config::app_cache_dir();
    let mut previous_torrents = config
        .rebase_cache_from
        .as_ref()
        .map(|previous| previous.join("torrents"));
    if let Some(raw) = crate::storage::try_get_str("torrent_settings")? {
        let mut settings: nova_config::TorrentSettings = serde_json::from_str(&raw)?;
        if !settings.dir.trim().is_empty() {
            previous_torrents = Some(PathBuf::from(settings.dir.trim()));
            settings.dir = cache.join("torrents").to_string_lossy().into();
        }
        crate::storage::try_set_str("torrent_settings", &serde_json::to_string(&settings)?)?;
    }
    // The engine adopts these paths independently of torrent_settings. Leaving
    // an original directory here would bypass fixture isolation during eviction.
    if let Some(raw) = crate::storage::try_get_str("torrent_cache")? {
        let mut tracked: BTreeMap<String, Value> = serde_json::from_str(&raw)?;
        for (index, entry) in tracked.values_mut().enumerate() {
            let source = PathBuf::from(
                entry["dir"]
                    .as_str()
                    .ok_or("invalid tracked torrent path")?,
            );
            let target = relocated_path(
                &source,
                previous_torrents.as_deref(),
                &cache.join("torrents"),
                &cache.join("torrents").join(format!("relocated-{index}")),
            );
            if source != target {
                copy_sparse_tree(&source, &target)?;
            }
            entry["dir"] = serde_json::to_value(target)?;
        }
        crate::storage::try_set_str("torrent_cache", &serde_json::to_string(&tracked)?)?;
    }
    Ok(())
}

fn copy_sparse_tree(source: &Path, target: &Path) -> Result<()> {
    if source.is_symlink() || !source.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = target.join(entry.file_name());
        if kind.is_dir() {
            copy_sparse_tree(&entry.path(), &path)?;
        } else if kind.is_file() {
            std::fs::File::create(path)?.set_len(entry.metadata()?.len())?;
        }
    }
    Ok(())
}

fn relocated_artifact(
    source: &Path,
    previous: Option<&Path>,
    data: &Path,
    index: usize,
) -> PathBuf {
    relocated_path(
        source,
        previous,
        data,
        &data
            .join("downloads/http")
            .join(format!("relocated-{index}")),
    )
}

fn relocated_path(source: &Path, previous: Option<&Path>, root: &Path, fallback: &Path) -> PathBuf {
    if source.strip_prefix(root).is_ok_and(safe_relative_path) {
        return source.to_path_buf();
    }
    if let Some(relative) = previous.and_then(|previous| source.strip_prefix(previous).ok())
        && safe_relative_path(relative)
    {
        return root.join(relative);
    }
    fallback.join(source.file_name().unwrap_or_default())
}

fn safe_relative_path(path: &Path) -> bool {
    path.components()
        .all(|part| matches!(part, std::path::Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shell_frame_does_not_qualify_as_ready_and_duplicate_frames_keep_first_times() {
        let mut data = Measurements::default();
        data.rendered(20, false, None);
        assert_eq!(data.milestones_us.get("first_render"), Some(&20));
        assert!(!data.milestones_us.contains_key("home_ready_render"));
        data.models_ready = true;
        data.rendered(40, false, None); // a valid empty Home needs no artwork
        data.rendered(60, true, None);
        assert_eq!(data.milestones_us.get("first_render"), Some(&20));
        assert_eq!(data.milestones_us.get("home_ready_render"), Some(&40));
        assert_eq!(data.milestones_us.get("featured_art_render"), Some(&60));
        assert!(!data.milestones_us.contains_key("navigation_render"));
        data.navigation_started = true;
        data.rendered(80, true, Some(Navigation::Library));
        assert_eq!(data.milestones_us.get("navigation_render"), Some(&80));
    }

    #[test]
    fn relocated_downloads_never_reference_the_original_directory() {
        let destination = Path::new("/bench/data/nova");
        assert_eq!(
            relocated_artifact(
                Path::new("/original/downloads/http/job/video.mp4"),
                Some(Path::new("/original")),
                destination,
                0
            ),
            destination.join("downloads/http/job/video.mp4")
        );
        assert_eq!(
            relocated_artifact(
                Path::new("/external/video.mp4"),
                Some(Path::new("/original")),
                destination,
                7
            ),
            destination.join("downloads/http/relocated-7/video.mp4")
        );
        let owned = destination.join("downloads/http/job/video.mp4");
        assert_eq!(relocated_artifact(&owned, None, destination, 0), owned);
        assert_eq!(
            relocated_artifact(
                Path::new("/original/../outside/video.mp4"),
                Some(Path::new("/original")),
                destination,
                8
            ),
            destination.join("downloads/http/relocated-8/video.mp4")
        );
    }

    #[test]
    fn tracked_torrents_relocate_custom_directories_and_keep_owned_paths() {
        let root = Path::new("/bench/cache/nova/torrents");
        let original = Path::new("/custom/torrents/hash");
        assert_eq!(
            relocated_path(original, Some(Path::new("/custom/torrents")), root, root),
            root.join("hash")
        );
        let owned = root.join("hash");
        assert_eq!(relocated_path(&owned, Some(root), root, root), owned);
    }
}

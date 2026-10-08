use addons::{Addon, Manifest, MetaItem, MetaPreview, Video};
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, Image, Model, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
#[cfg(feature = "desktop")]
use std::sync::mpsc;
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::Duration;

use crate::download::DownloadJob;
use crate::{
    AddonRow, AppWindow, CalCell, CategoryRow, ContinueRow, DownloadRow, EpisodeRow,
    HomeCatalogCard, HomeCatalogRow, HomeCatalogSection, LicenseSource, MediaCard, SeasonCard,
    SheetItem, StreamRow, SyncInvite, SyncPeer, UpcomingRow,
};

pub(crate) const OPEN_SOURCE_LICENSE_CATALOG: &str =
    include_str!(concat!(env!("OUT_DIR"), "/nova_license_catalog.txt"));

pub(crate) fn open_source_license_sources() -> Vec<LicenseSource> {
    const WORKSPACE_CRATES: [&str; 12] = [
        "nova",
        "addons",
        "nova-providers",
        "nova-ui",
        "nova-storage",
        "nova-config",
        "nova-download",
        "nova-torrent",
        "nova-media",
        "nova-player",
        "nova-sync",
        "nova-tracking",
    ];

    let mut sources = include_str!(concat!(env!("OUT_DIR"), "/nova_license_sources.tsv"))
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let label = fields.next()?;
            let license = fields.next()?;
            let url = fields.next()?;
            if label.is_empty() {
                return None;
            }
            let package_name = label.split_whitespace().next()?;
            if WORKSPACE_CRATES.contains(&package_name) {
                return None;
            }
            let url = if url.starts_with("https://") {
                url.to_string()
            } else {
                let mut crate_parts = label.split_whitespace();
                let name = crate_parts.next()?;
                let version = crate_parts.next()?;
                format!("https://crates.io/crates/{name}/{version}")
            };
            Some(LicenseSource {
                label: label.into(),
                license: license.into(),
                url: url.into(),
            })
        })
        .collect::<Vec<_>>();

    sources.sort_by_key(|a| a.label.to_lowercase());
    sources.dedup_by(|a, b| a.label == b.label && a.url == b.url);
    sources
}
use crate::net;
use crate::storage;
use nova_config::{
    AndroidHwdec, BackdropSize, CacheImageFormat, CacheSettings, DesktopExternalApp,
    DownloadSettings, EpisodeStartBehavior, HomeCatalogSource, HomeRow, HomeRowSource, Language,
    active_cache_settings, app_cache_dir, app_data_dir, now_secs, poster_cache_dir,
    set_cache_settings,
};
// Everything from the image-cache subsystem now lives in `nova-media`.
use nova_media::cache::*;
// Re-exported so `nova::app::TorrentSettings` stays a valid path.
pub use nova_config::TorrentSettings;

// ---------------------------------------------------------------------------
// State shared between UI callbacks and background worker threads.
// ---------------------------------------------------------------------------

/// A successfully installed addon (manifest cached for picker building).
#[derive(Clone)]
pub(crate) struct Installed {
    /// Normalised base install URL.
    url: String,
    /// `manifest.name`, made unique among installed addons.
    label: String,
    /// Whether the addon is enabled. Disabled addons stay installed (and
    /// listed in Settings → Addons) but are hidden from Discover and no
    /// longer provide catalogs, meta or streams until re-enabled.
    enabled: bool,
    /// Whether `<base>/configure` answered 2xx. `None` = never checked.
    /// Written down to the KV store once known (see `persist_installed`),
    /// so each addon is probed once per lifetime, not every launch.
    configure_ok: Option<bool>,
    manifest: Manifest,
    /// Desired entries exist even when their manifest is unavailable.
    available: bool,
    generation: u64,
}

struct CatDef {
    id: String,
    supports_skip: bool,
    genre_options: Vec<String>,
    label: String,
    /// Which addon this catalog belongs to (its label). Empty when only one
    /// addon is active (not needed for disambiguation).
    addon_label: String,
}

/// One enabled addon catalog that accepts a global search query. Pagination
/// is tracked per target because catalogs can differ in both support and
/// progress.
#[derive(Clone)]
struct SearchTarget {
    addon_url: String,
    type_: String,
    catalog_id: String,
    genre_options: Vec<String>,
    genre: String,
    supports_skip: bool,
    next_skip: usize,
    exhausted: bool,
    /// All pages received from this source, kept in source order so partial
    /// fan-out responses can be merged deterministically as they arrive.
    results: Vec<MetaPreview>,
}

/// Search selections use stable source identities rather than browse indices.
/// They last until Back closes the search and never enter persisted settings.
#[derive(Clone, Default, PartialEq, Eq)]
struct SearchFilters {
    addon_url: String,
    type_: String,
    catalog: Option<SearchCatalogSource>,
    genre: String,
}

#[derive(Clone, PartialEq, Eq)]
struct SearchCatalogSource {
    addon_url: String,
    type_: String,
    catalog_id: String,
}

impl SearchTarget {
    fn source(&self) -> SearchCatalogSource {
        SearchCatalogSource {
            addon_url: self.addon_url.clone(),
            type_: self.type_.clone(),
            catalog_id: self.catalog_id.clone(),
        }
    }
}

pub(crate) struct TypeDef {
    type_: String,
    label: String,
    catalogs: Vec<CatDef>,
}

/// How a stream row can be played.
#[derive(Clone)]
pub(crate) enum StreamSource {
    /// A direct URL (HLS/MP4/…) playable by mpv as-is.
    Url(String),
    /// A direct URL with addon-supplied headers or external subtitles.
    UrlWithOptions {
        url: String,
        headers: Vec<(String, String)>,
        subtitles: Vec<String>,
    },
    /// A torrent row (`infoHash` + optional `fileIdx`) that the embedded
    /// BitTorrent engine resolves to a loopback URL.
    #[allow(dead_code)]
    Torrent {
        info_hash: String,
        file_idx: Option<u32>,
    },
    /// Nothing playable here (e.g. a YouTube-only stream with no direct URL).
    Unsupported,
    /// A completed local download represented in the stream list.
    Downloaded { job_id: String, path: PathBuf },
}

/// Resolved playback target held while the user chooses whether to resume.
#[derive(Clone)]
pub(crate) struct PendingStream {
    pub(crate) url: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) subtitles: Vec<String>,
}

/// One row shown in the modal's stream list.
#[derive(Clone)]
struct StreamUi {
    id: String,
    display: String,
    source: StreamSource,
    /// Label of the addon that returned this stream, for the per-addon
    /// filter pills above the list (empty when the addon is unknown).
    addon: String,
    download: Option<DownloadJob>,
}

/// Decoded artwork retained for an item in Home's small featured carousel.
/// Failed URLs are cooled down rather than becoming permanent session misses.
/// Existing pixels stay visible while richer metadata/artwork is loaded.
#[derive(Default)]
struct HomeShowcaseArtwork {
    backdrop: Option<SharedPixelBuffer<Rgba8Pixel>>,
    backdrop_url: String,
    logo: Option<SharedPixelBuffer<Rgba8Pixel>>,
    logo_url: String,
    logo_loading: bool,
    logo_failed_at: Option<std::time::Instant>,
    backdrop_done: bool,
    loading_url: Option<String>,
    failed_urls: HashMap<String, std::time::Instant>,
    metadata_loading: bool,
    metadata_revision: Option<u64>,
    metadata_retry_at: Option<std::time::Instant>,
}

struct MetadataPrefetch {
    revision: u64,
    /// None while queued or fetching; repeated Library visits must not
    /// start another chain before the first one has completed.
    retry_at: Option<std::time::Instant>,
}

#[derive(Clone)]
struct HomeCatalogGroup {
    source: HomeCatalogSource,
    title: String,
    previews: Vec<MetaPreview>,
}

#[derive(Default)]
struct Shared {
    installed: Vec<Installed>,
    chosen_addon: usize,
    type_defs: Vec<TypeDef>,
    chosen_type: usize,
    chosen_catalog: usize,
    chosen_genre: String,
    search: String,
    /// True while the initial set of addons is being loaded at startup.
    /// Suppresses the "All addons are disabled" hint until loading finishes.
    loading_addons: bool,
    /// Items offset for the next page in endless-scroll pagination.
    next_skip: usize,
    /// Set when a page adds 0 new items (no more pages to fetch).
    catalog_exhausted: bool,
    /// In-flight guard: true while an append-fetch is in flight.
    loading_more: bool,
    /// Generation of the currently displayed catalog (posters / stale drops).
    catalog_gen: u64,
    /// MetaPreviews backing the current grid, index-aligned with `catalog`.
    previews: Vec<MetaPreview>,
    /// Search result set, kept separate so returning from Search restores the
    /// selected browse catalog without another request.
    search_previews: Vec<MetaPreview>,
    search_filters: SearchFilters,
    search_targets: Vec<SearchTarget>,
    search_generation: u64,
    /// Requests still outstanding in the current search or pagination wave.
    search_pending_requests: usize,
    search_loading_more: bool,
    search_poster_inflight: HashSet<(u64, String, String)>,
    /// Cooldown prevents failed saved artwork from flooding metadata requests.
    artwork_recovery: HashMap<(String, String, String), std::time::Instant>,
    /// Confirmed missing/failed URLs awaiting usable addon manifests.
    pending_artwork_recovery: HashSet<(String, String, String)>,
    library_poster_inflight: HashSet<(String, String, String)>,
    /// Session-only refresh state, independent of legacy header/episode caches.
    metadata_prefetch: HashMap<(String, String), MetadataPrefetch>,
    /// Saved library items (My Library), order = insertion order.
    entries: Vec<LibraryEntry>,
    /// Current image-cache settings (mirrored from the KV store).
    cache_settings: CacheSettings,
    download_settings: DownloadSettings,
    /// Modal target, if open.
    modal_item: Option<ModalItem>,
    /// Streams currently displayed (already narrowed by `stream_filter`).
    streams: Vec<StreamUi>,
    /// Every stream returned for the current request, before filtering.
    stream_all: Vec<StreamUi>,
    /// A metadata refresh can rerun the same episode with new stream aliases.
    /// Older addon replies must not modify the new search's rows or pills.
    stream_generation: u64,
    /// Labels of the addons shown as filter pills, in installed-addon order.
    /// Pending addons are included until they answer (then dropped if they
    /// returned nothing).
    stream_addons: Vec<String>,
    /// Labels of the addons still being queried (pills show a spinner).
    stream_pending: Vec<String>,
    /// A one-off, localized stream hint. Derived stream-list hints are
    /// rebuilt from `stream_pending`/`stream_all`; this stores only hints whose
    /// source is an action or status outside that model.
    stream_hint: Option<StreamHint>,
    /// Active per-addon filter: 0 = All, otherwise `index + 1` into
    /// `stream_addons`.
    stream_filter: usize,
    /// Current page in the filtered stream list (25 rows per page).
    stream_page: usize,
    /// Episode playback history (watched + resume positions), mirrored in
    /// the KV store under [`EPISODE_PROGRESS_KEY`].
    progress: HashMap<String, EpisodeProgress>,
    /// Continue Watching items the user removed from Home, with the removal
    /// time (`id -> unix secs`). Mirrored locally under
    /// [`CONTINUE_HIDDEN_KEY`] and synced as the `continue_hidden` domain. An
    /// item stays hidden until a newer progress update (a resume, local or
    /// from a peer) appears; see [`Bridge::rebuild_continue_list`].
    continue_hidden: HashMap<String, u64>,
    /// Home → Continue Watching entries, rebuilt from `progress` whenever
    /// it changes (latest resumable episode per library series first).
    continue_list: Vec<ContinueEntry>,
    /// Home → Upcoming entries, rebuilt alongside (`continue_list`):
    /// unaired episodes of caught-up library series, air-date first.
    upcoming_list: Vec<UpcomingEntry>,
    /// Catalog poster rails configured separately from the featured banner.
    home_catalog_row_groups: Vec<HomeCatalogGroup>,
    home_catalog_row_sources: Vec<HomeCatalogSource>,
    /// Flattened in the same order as the Home UI cards for click resolution.
    home_catalog_row_items: Vec<MetaPreview>,
    home_catalog_rows_loaded: bool,
    /// Selected catalog previews for Home's independent featured showcase.
    home_showcase: Vec<MetaPreview>,
    home_showcase_sources: Vec<HomeCatalogSource>,
    /// Successful refreshes wait here until the carousel advances. The
    /// displayed preview remains the action target while new art is loading.
    home_showcase_refresh: Option<Vec<MetaPreview>>,
    home_showcase_displayed: Option<MetaPreview>,
    home_showcase_list_generation: u64,
    home_showcase_index: usize,
    home_showcase_pending_index: Option<usize>,
    home_showcase_artwork: HashMap<usize, HomeShowcaseArtwork>,
    home_showcase_loaded: bool,
    /// Home → Upcoming calendar month/selection/open flag (UI-only).
    upcoming_cal: UpcomingCal,
    /// Episode currently playing (series flow); `None` for movies / idle.
    playback: Option<PlaybackTarget>,
    /// Resolved stream held while the user chooses whether to resume.
    pending_resume_stream: Option<PendingStream>,
    resume_prompt_choice: Option<bool>,
    /// Info hash of the torrent currently being streamed, if any. Set when a
    /// torrent resolves and opens the player; cleared when the player closes.
    /// Drives the "Connecting… ↓ rate · peers · %" status line.
    active_torrent: Option<String>,
    /// In-flight per-addon metadata refreshes (base URLs). Guards the
    /// Refresh button against double-clicks racing two manifest fetches.
    refreshing: HashSet<String>,
    /// Detail-view snapshots keyed by item id (in-memory only): backing out
    /// of an entry and reopening it restores tab, season, focused
    /// episode/stream and keyboard focus instead of starting fresh.
    detail_snapshots: HashMap<String, DetailSnapshot>,
    /// Most recent sync notice, retained in semantic form so a language switch
    /// can rerender it instead of leaving the old translation on screen.
    sync_link_notice: Option<SyncNotice>,
}

#[derive(Clone, Debug)]
enum StreamHint {
    Fixed(&'static str),
    LoadingEpisodes(usize),
    Playing(String),
    P2pUnavailable(String),
}

impl StreamHint {
    fn render(&self) -> String {
        match self {
            Self::Fixed(source) => text::tr(source).to_string(),
            Self::LoadingEpisodes(count) => text::loading_episodes(*count),
            Self::Playing(name) => text::playing(name),
            Self::P2pUnavailable(error) => text::p2p_unavailable(error),
        }
    }
}

#[derive(Clone, Debug)]
enum SyncNotice {
    Fixed(&'static str),
    PairedWith(String),
    RemovedFromSync(Option<String>),
    CouldNotCreateInvite(String),
    InvalidInvite(String),
}

impl SyncNotice {
    fn render(&self) -> String {
        match self {
            Self::Fixed(source) => text::tr(source).to_string(),
            Self::PairedWith(name) => text::paired_with(name),
            Self::RemovedFromSync(Some(name)) => text::removed_from_sync(name),
            Self::RemovedFromSync(None) => text::removed_from_sync(text::tr("another device")),
            Self::CouldNotCreateInvite(error) => text::could_not_create_invite(error),
            Self::InvalidInvite(error) => text::invalid_invite(error),
        }
    }
}

/// Determines Back behavior after Watch Now opens an episode directly.
#[derive(Clone, Copy)]
enum WatchNowOrigin {
    Featured,
    Detail,
}

/// The item whose detail modal is currently open.
struct ModalItem {
    /// Identity of this opening, including reopens of the same title.
    open_token: Arc<()>,
    /// One-shot selection intent; never persisted or carried to another modal.
    pending_watch_now: Option<WatchNowOrigin>,
    episodes_loading: bool,
    /// The item's own id (movie or series id).
    id: String,
    type_: String,
    /// Id of the last stream request: the item id for movies, or the
    /// episode id (`<item>:<season>:<episode>`) for a picked episode.
    request_id: String,
    /// Episodic metadata (series only): the videos the meta provider
    /// returned. Empty for movies / when no provider exists.
    videos: Vec<Video>,
    /// Season-specific backdrops from addon metadata, keyed by season number.
    season_backdrops: HashMap<u32, String>,
    /// Distinct seasons in display order (numbered first, extras last).
    seasons: Vec<u32>,
    /// Index into `seasons` currently shown in the episode picker.
    season_index: usize,
    /// Episodes tab page (0-based) of the open modal; the season's filtered
    /// list is sliced into `EPISODE_PAGE_SIZE`-row pages.
    episode_page: usize,
    /// Display name / year snapshots, kept for the detail page and for
    /// saving the item to the library without another meta fetch.
    name: String,
    year: String,
    /// Poster URL as served by the catalog addon (may be empty).
    poster_url: String,
    /// Wide backdrop URL (`background` in the addon protocol; may be empty).
    background_url: String,
    /// Transparent show/movie title artwork; empty keeps the text heading.
    logo_url: String,
    /// Item description (`description` in the addon protocol; may be empty).
    description: String,
    /// Genre list (`genres` in the addon protocol; may be empty).
    genres: Vec<String>,
}

/// Detail-view position snapshot for one item: written on modal close,
/// consumed when the same entry is reopened so Back returns to the same
/// previous state (tab, season, focused episode/stream, keyboard focus).
/// Season is stored by value and episodes by id so list refreshes or
/// reorderings resolve gracefully (fall back to defaults when absent).
#[derive(Clone, Debug, Default)]
struct DetailSnapshot {
    tab: i32,
    season: Option<u32>,
    /// Focused episode row on the list (video id).
    episode_id: Option<String>,
    /// Stream-list focus: index valid for `stream_request`.
    stream_idx: usize,
    stream_request: String,
    filter: String,
    kb_zone: i32,
    kb_top: i32,
    kb_tab: i32,
    kb_ci: i32,
    kb_ep: i32,
    kb_s: i32,
}

// ---------------------------------------------------------------------------
// Episode playback tracking (watched + resume position + full history).
// ---------------------------------------------------------------------------

/// One episode's playback state, persisted in the KV store under
/// `EPISODE_PROGRESS_KEY` as a `HashMap<String, EpisodeProgress>` keyed by
/// [`progress_map_key`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct EpisodeProgress {
    series_id: String,
    episode_id: String,
    /// Last observed playback position in seconds.
    position_secs: f64,
    /// Duration in seconds as last reported by the player (0 = unknown).
    duration_secs: f64,
    /// Sticky once set: position/duration crossed [`WATCHED_FRACTION`] or the
    /// stream played to its natural end.
    watched: bool,
    /// Last explicit unwatch (unix secs, 0 = none). Lets the mesh merge tell
    /// "user marked unwatched" apart from "stale record that never reached
    /// the end": an unwatch intent newer than the other side's state wins,
    /// otherwise watched sticks. Cleared whenever watched is (re)set.
    #[serde(default)]
    unwatched_at_secs: u64,
    /// How many times playback of this episode was opened.
    play_count: u32,
    /// Seconds since the Unix epoch of the last update.
    updated_at_secs: u64,
}

/// In-memory target of the currently playing episode (series flow only;
/// movies clear it). Lives in [`Shared::playback`].
#[derive(Clone, Debug, Default)]
struct PlaybackTarget {
    series_id: String,
    episode_id: String,
    /// Position the engine should open at (resume). `None` when starting
    /// fresh / already watched / no saved position.
    resume_pos: Option<f64>,
    /// The tick safety net is done with this resume point: the engine either
    /// landed at it or the corrective seek was issued. Set once, so a later
    /// user scrub is never pulled back to the saved position.
    resume_done: bool,
    /// Last position/duration observed while the player was open (used to
    /// finalize when the player closes, since close resets the UI props).
    last_pos: f64,
    last_dur: f64,
    /// Last wall-clock save (throttles KV writes during the 250 ms poll).
    last_saved_at: u64,
    last_saved_pos: f64,
}

/// One Home → Continue Watching entry: the latest resumable episode of a
/// library series. Rebuilt from [`Shared::progress`] by
/// [`Bridge::rebuild_continue_list`]; display data (title/poster/label)
/// is joined at render time so library edits apply without a rebuild.
#[derive(Clone, Debug, Default)]
struct ContinueEntry {
    series_id: String,
    type_: String,
    episode_id: String,
    updated_at_secs: u64,
}

/// One Home → Upcoming entry: an unaired episode of a series whose
/// available episodes are all watched. Rebuilt by
/// [`Bridge::rebuild_upcoming_list`]; air-date ascending.
#[derive(Clone, Debug, Default)]
struct UpcomingEntry {
    series_id: String,
    type_: String,
    episode_id: String,
    air_days: i64,
}

/// Home → Upcoming calendar state (UI-only: never synced or persisted).
/// The visible month (`first` = epoch days of its 1st, 0 = uninitialized)
/// plus the selected day. Healed by [`Bridge`] whenever the Upcoming list
/// changes; see `home.rs`.
#[derive(Clone, Debug, Default)]
struct UpcomingCal {
    open: bool,
    first: i64,
    day: Option<i64>,
}

/// KV key holding the whole progress map.
const EPISODE_PROGRESS_KEY: &str = "episode_progress";
/// KV key holding the Continue Watching hide map (`id -> hidden_at unix secs`).
/// Mirrors the synced `continue_hidden` domain so the choice survives with sync
/// off. Local storage stays JSON, like progress.
const CONTINUE_HIDDEN_KEY: &str = "continue_hidden";
/// Episodes per page in the detail modal's Episodes tab: a 200-episode season
/// renders (and fetches thumbnails for) one page at a time instead of
/// instantiating every card at once.
const EPISODE_PAGE_SIZE: usize = 50;
/// Fraction of `position / duration` at which an episode counts as watched.
const WATCHED_FRACTION: f64 = 0.90;
/// Saved positions below this are treated as "no resume" (cold opens,
///
/// false starts).
const RESUME_MIN_SECS: f64 = 10.0;
/// Minimum wall-clock gap between progress writes during playback. Only
/// bounds kill-loss granularity: watched flips, large seeks and
/// finalize-on-close save immediately, so 30 s loses nothing that matters
/// while cutting KV + mesh churn versus a tighter loop.
const PROGRESS_SAVE_INTERVAL_SECS: u64 = 30;
/// Minimum position delta that forces a write even within the interval.
const PROGRESS_MIN_DELTA_SECS: f64 = 5.0;

#[cfg(feature = "desktop")]
/// Per-(generation, index) decoded poster store backing the grid + detail
/// fast paths. Entries are deliberately NOT scoped to the current
/// generation — going back to a previous search/addon must stay instant —
/// so the store is globally capped instead (oldest-inserted evicted first).
/// Evicted entries transparently fall back to the decoded LRU / disk cache.
#[cfg(feature = "desktop")]
struct PosterStore {
    map: HashMap<(u64, usize), SharedPixelBuffer<Rgba8Pixel>>,
    /// Insertion order (oldest first) for cap eviction.
    order: VecDeque<(u64, usize)>,
    cap: usize,
}

#[cfg(feature = "desktop")]
impl PosterStore {
    fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    fn get(&self, key: &(u64, usize)) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
        self.map.get(key).cloned()
    }

    fn insert(&mut self, key: (u64, usize), pixels: SharedPixelBuffer<Rgba8Pixel>) {
        if !self.map.contains_key(&key) {
            self.order.push_back(key);
        }
        self.map.insert(key, pixels);
        while self.map.len() > self.cap {
            match self.order.pop_front() {
                Some(old) => {
                    self.map.remove(&old);
                }
                None => break,
            }
        }
    }

    /// Drop one entry, if present (used to invalidate a grid card whose
    /// poster art changed upstream).
    fn remove(&mut self, key: &(u64, usize)) {
        if self.map.remove(key).is_some() {
            self.order.retain(|k| k != key);
        }
    }

    fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(feature = "desktop")]
type PosterCache = Arc<Mutex<PosterStore>>;

#[cfg(feature = "desktop")]
struct PosterJob {
    generation: u64,
    index: usize,
    url: String,
    /// True when the card lives in the Library grid (setter differs).
    library: bool,
}

/// Dual-priority poster intake (desktop only).
///
/// The Slint grids report a *fetch window* of visible ±1–2 rows
/// (`visible_range` / `library_visible_range`). Re-fetches for that window
/// are near-viewport and latency-sensitive, so they go on `hi`. The initial
/// full-grid sweeps (every card, eventually) go on `lo`. Workers drain `hi`
/// first, so a fast scroll never waits behind hundreds of far-away jobs.
#[cfg(feature = "desktop")]
#[derive(Clone)]
struct PosterTx {
    hi: mpsc::Sender<PosterJob>,
    lo: mpsc::Sender<PosterJob>,
}

#[cfg(feature = "desktop")]
impl PosterTx {
    fn send_hi(&self, job: PosterJob) {
        let _ = self.hi.send(job);
    }

    fn send_lo(&self, job: PosterJob) {
        let _ = self.lo.send(job);
    }
}

/// Episode thumbnail URLs currently being fetched, so repeated row refreshes
/// don't spawn duplicate downloads for the same image.
static EPISODE_INFLIGHT: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// A queued episode-thumbnail fetch. Completed pixels are cached by URL;
/// the debounced render always reads the currently visible episode list.
struct EpisodeThumbJob {
    url: String,
    /// Refresh check (not a plain fetch): the URL is already cached, so
    /// fresh bytes download in the background and the pixels swap only
    /// when they actually changed — unchanged art never flashes through
    /// the placeholder.
    verify: bool,
}

/// Thumbnails waiting for a free download slot. Concurrency is capped so a
/// season with hundreds of episodes never spawns one thread per image.
static EPISODE_QUEUE: LazyLock<Mutex<VecDeque<EpisodeThumbJob>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

static EPISODE_ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// True while a debounced episode-list render is already scheduled.
static EPISODE_RENDER_PENDING: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Bridge: all UI-mutating methods run on the main thread only.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Bridge {
    app: slint::Weak<AppWindow>,
    shared: Arc<Mutex<Shared>>,
    catalog_gen: Arc<AtomicU64>,
    /// Stale-response guard for the independent Home showcase fetches.
    home_showcase_gen: Arc<AtomicU64>,
    /// Stale-response guard for Home's additional catalog poster rails.
    home_catalog_rows_gen: Arc<AtomicU64>,
    // Native-only poster worker pipeline (dual-priority channels, see
    // `PosterTx`). Android fetches each grid poster via net::fetch_image instead.
    #[cfg(feature = "desktop")]
    poster_tx: PosterTx,
    #[cfg(feature = "desktop")]
    poster_cache: PosterCache,
    /// In-app player overlay (same window; mpv on desktop, HTML5 video on web).
    player: crate::player::Player,
    downloads: DownloadCoordinator,
    /// Bounded tracker actor; unavailable stores retain their originals and
    /// require explicit recovery without blocking playback.
    tracking: tracking::StateHandle,
    downloads_seen: Arc<AtomicU64>,
    stream_seq: Arc<AtomicU64>,
}

mod addon_mgr;
mod bridge;
mod catalog;
mod detail;
mod downloads;
mod home;
mod library;
mod playback;
mod posters;
mod run;
mod settings;
mod settings_sync;
pub use run::run;

mod qr;

mod clipboard;
mod episodes;
mod i18n;
mod io;
mod streams;
mod sync;
mod tracking;
pub(crate) use nova_ui::backend_text as text;

// Android background execution glue (foreground service + JobScheduler sync).
#[cfg(target_os = "android")]
pub(crate) mod android_bg;

// Android camera QR scanner glue (QrScanActivity + rqrr decode).
#[cfg(target_os = "android")]
pub(crate) mod android_qr;

// Android player-gesture system bridges (swipe volume/brightness).
#[cfg(target_os = "android")]
pub(crate) mod android_player;

pub(crate) use addon_mgr::*;
pub(crate) use catalog::*;
pub(crate) use clipboard::*;
pub(crate) use detail::*;
pub(crate) use downloads::*;
pub(crate) use episodes::*;
pub(crate) use home::*;
pub(crate) use i18n::*;
pub(crate) use io::*;
pub(crate) use library::*;
#[cfg(test)]
pub(crate) use nova_ui::backend_text::grouped_count;
pub(crate) use playback::*;
pub(crate) use posters::*;
pub(crate) use qr::*;
pub(crate) use settings::*;
pub(crate) use streams::*;
pub(crate) use sync::*;

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Persistence: the installed addon list + per-addon manifest cache live in
// the KV store. Manifests are re-fetched only when an addon is installed fresh
// or the cache misses.
// ---------------------------------------------------------------------------

/// Wire format for addon entries in the KV store (and the synced record).
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct AddonStore {
    pub url: String,
    pub enabled: bool,
    /// Persisted configure-page verdict (`None` = never checked; missing in
    /// entries written by older versions, which then probe once).
    #[serde(default)]
    pub configure_ok: Option<bool>,
    /// Friendly display label. Synced so every device shows the same name;
    /// empty in older saves, where it is derived from the manifest.
    #[serde(default)]
    pub label: String,
}

// ---------------------------------------------------------------------------
// Library persistence ("My Library", own collection of movies/series/anime).
// ---------------------------------------------------------------------------

/// Manual watch status for a library item. `Auto` (default) derives the
/// bucket from playback progress; `OnHold`/`Dropped` pin the item to that
/// bucket regardless of progress. Set from the library card context menu.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) enum WatchStatus {
    /// Derive from progress: Plan to Watch / Watching / Completed.
    #[default]
    Auto,
    OnHold,
    Dropped,
}

impl WatchStatus {
    /// Badge label shown on the card while pinned (`None` when automatic).
    fn badge_label(self) -> Option<&'static str> {
        match self {
            WatchStatus::Auto => None,
            WatchStatus::OnHold => Some("On Hold"),
            WatchStatus::Dropped => Some("Dropped"),
        }
    }
}

/// Built-in (automatic) library filters, listed before user categories in
/// the filter rail. "All" (empty filter) still shows everything.
const BUILTIN_FILTERS: &[&str] = &[
    "Watching",
    "Completed",
    "On Hold",
    "Dropped",
    "Plan to Watch",
];

/// One saved library item. `type_` is the meta type as reported by the
/// catalog addon ("movie", "series", … anime catalogs usually report
/// "series"); the same value is used later to match stream addons.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct LibraryEntry {
    /// The item's own id (movie / series / anime id as given by the meta
    /// addon), used to de-duplicate and to re-fetch metadata.
    id: String,
    type_: String,
    /// Display name snapshot (kept so the library grid renders offline).
    name: String,
    #[serde(default)]
    year: String,
    /// Poster URL, re-used to render the grid (bytes are file-cached).
    #[serde(default)]
    poster_url: String,
    /// Wide backdrop URL (`background` in the addon protocol), re-used to
    /// render the detail header instantly on reentry (pixels stay file- and
    /// LRU-cached; without the URL a library reopen must wait for a meta
    /// fetch before the image load can even start).
    #[serde(default)]
    background_url: String,
    /// Genre list snapshot (`genres` in the addon protocol), painted
    /// synchronously on library reopen so pills don't wait for a meta fetch.
    #[serde(default)]
    genres: Vec<String>,
    /// Item synopsis snapshot (`description` in the addon protocol), painted
    /// synchronously on library reopen for the same reason.
    #[serde(default)]
    description: String,
    /// User-defined category tags (e.g. "Action", "Must Watch").
    #[serde(default)]
    pub categories: Vec<String>,
    /// Manual watch-status pin (On Hold / Dropped); `Auto` derives the
    /// bucket from playback progress. Defaults for older saves.
    #[serde(default)]
    pub watch_status: WatchStatus,
    /// Wall-clock seconds of when the item was added. Gives the library a
    /// stable order that is identical on every device after a sync (the list
    /// is sorted by this, ties broken by id). `0` for entries saved before
    /// sync existed; backfilled from the saved order at startup.
    #[serde(default)]
    pub added_at_secs: u64,
}

// ---------------------------------------------------------------------------
// Settings (image cache re-encoding), persisted in the KV store.
// ---------------------------------------------------------------------------

/// KV key holding the persisted torrent settings (JSON), alongside every
/// other durable setting.
#[allow(dead_code)]
const TORRENT_SETTINGS_KEY: &str = "torrent_settings";

/// Runtime torrent settings (kept in sync with the KV store); read by the
/// torrent engine and the player-close cleanup path.
static CURRENT_TORRENT_SETTINGS: LazyLock<Mutex<TorrentSettings>> =
    LazyLock::new(|| Mutex::new(TorrentSettings::default()));

// ---------------------------------------------------------------------------
// Season / episode metadata cache (series & anime).
// ---------------------------------------------------------------------------

/// Prefetched detail-header snapshot from a `meta` response (`background` /
/// `description` / `genres` / year). The episode prefetch already fetches
/// full `MetaItem`s but used to discard the header and keep only `videos`,
/// so descriptions + genre pills still needed a network round-trip on open.
/// Stored under `meta_header:{type}\x01{id}`; empty slots mean "unknown",
/// never "known empty". Text merges fill gaps; season artwork accepts fresh URLs.
/// Poster URLs are recorded only after successful image decoding and reused by Discover.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct MetaHeader {
    #[serde(default)]
    poster_url: String,
    #[serde(default)]
    background_url: String,
    #[serde(default)]
    logo_url: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    genres: Vec<String>,
    #[serde(default)]
    year: String,
    #[serde(default)]
    season_backdrops: HashMap<u32, String>,
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const DEFAULT_ADDON: &str = "https://v3-cinemeta.strem.io";

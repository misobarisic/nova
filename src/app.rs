use addons::{Addon, Manifest, MetaItem, MetaPreview, Video};
use profiles::thread;
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, Image, Model, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::collections::{HashMap, HashSet, VecDeque};
#[cfg(any(feature = "desktop", test))]
use std::fs;
#[cfg(feature = "desktop")]
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
#[cfg(feature = "desktop")]
use std::sync::mpsc;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use crate::download::DownloadJob;
use crate::{
    AddonRow, AppWindow, CalCell, CategoryRow, ContinueRow, DownloadRow, EpisodeRow,
    HomeCatalogCard, HomeCatalogRow, HomeCatalogSection, LicenseSource, MediaCard, SeasonCard,
    SheetItem, StreamRow, SyncInvite, SyncPeer, UpcomingRow,
};

pub(crate) const OPEN_SOURCE_LICENSE_CATALOG_ZSTD: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/nova_license_catalog.zst"));
include!(concat!(env!("OUT_DIR"), "/nova_license_catalog_size.rs"));

pub(crate) fn open_source_license_sources() -> Vec<LicenseSource> {
    const WORKSPACE_CRATES: [&str; 17] = [
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
        "nova-playback-api",
        "nova-cast",
        "nova-cast-media",
        "nova-remux",
        "nova-sync",
        "nova-tracking",
        "nova-stremio",
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
use nova_config::{
    AndroidHwdec, BackdropSize, CacheImageFormat, CacheSettings, DesktopExternalApp,
    DownloadSettings, EpisodeStartBehavior, HomeCatalogSource, HomeRow, HomeRowSource, Language,
    active_cache_settings, app_cache_dir, app_data_dir, image_settings_revision, now_secs,
    poster_cache_dir, set_cache_settings,
};
use profile_net as net;
use profiles::storage;
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
    /// Exact installation identity, including its configured path/query.
    addon_url: String,
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
    /// A provider web page, explicitly opened in the system browser.
    ExternalUrl(String),
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
    /// An installed bundled provider owns this non-secret selection token.
    /// A fresh playable URL is requested only after the user chooses the row.
    Resolver {
        provider_url: String,
        token: String,
        settings_generation: u64,
        addon_generation: u64,
    },
    /// Nothing playable here (e.g. a YouTube-only stream with no direct URL).
    Unsupported,
    /// A completed local download represented in the stream list.
    Downloaded { job_id: String, path: PathBuf },
}

/// Resolved playback target held while the user chooses whether to resume.
#[derive(Clone)]
pub(crate) struct PendingStream {
    pub(crate) request: crate::torrent::PlaybackRequest,
    pub(crate) url: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) subtitles: Vec<String>,
}

/// One row shown in the modal's stream list.
#[derive(Clone)]
struct StreamUi {
    id: String,
    /// Cross-request source fingerprint; excludes changing presentation text.
    choice_id: Option<String>,
    /// Transport identity, separate from the potentially shared display label.
    owner_url: String,
    display: String,
    card: StreamCardText,
    source: StreamSource,
    /// Label of the addon that returned this stream, for the per-addon
    /// filter pills above the list (empty when the addon is unknown).
    addon: String,
    download: Option<DownloadJob>,
}

/// Request state for an item in Home's featured carousel. Decoded pixels live
/// only in the shared image cache; the visible Slint image owns active pixels.
/// Failed URLs are cooled down rather than becoming permanent session misses.
#[derive(Default)]
struct HomeShowcaseArtwork {
    /// Last selected source URL. Pixels live only in the shared byte-budgeted cache.
    backdrop_url: String,
    logo_url: String,
    logo_loading: bool,
    logo_failed_at: Option<std::time::Instant>,
    loading_url: Option<String>,
    failed_urls: HashMap<String, std::time::Instant>,
    metadata_loading: bool,
    metadata_revision: Option<u64>,
    metadata_retry_at: Option<std::time::Instant>,
}

#[derive(Clone)]
struct MetadataPrefetch {
    revision: u64,
    /// None while queued or fetching; repeated Library visits must not
    /// start another chain before the first one has completed.
    retry_at: Option<std::time::Instant>,
}

#[derive(Clone)]
struct HomeCatalogGroup {
    source: HomeRowSource,
    title: String,
    previews: Vec<MetaPreview>,
}

#[derive(Default)]
struct Shared {
    bookmarks: bookmarks::State,
    viewing_journal: viewing_journal::JournalTracker,
    previously_watched: previously_watched::State,
    watch_queue: watch_queue::State,
    playback_tools: playback_tools::PlaybackToolsState,
    progress_projection: progress_projection::State,
    library_prefetch_preparing: bool,
    saved_episode_refresh_preparing: bool,
    similar_cache: Arc<Mutex<similar::Cache>>,
    recommendations: recommendations::State,
    addon_subtitles: subtitles::State,
    pending_subtitle_hints: Vec<(String, String)>,
    remote_playback: remote_playback::State,
    casting: casting::State,
    party: party::State,
    stream_prefetch: stream_prefetch::Cache,
    stream_preferences: stream_preferences::State,
    catalog_grouping: catalog_grouping::State,
    catalog_detail: catalog_detail::State,
    stream_prefetch_live_session: Option<u64>,
    installed: Vec<Installed>,
    addon_browser: addon_browser::State,
    package_descriptors: std::collections::BTreeMap<String, addon_packages::PackageDescriptor>,
    loaded_packages: std::collections::BTreeMap<String, Arc<nova_providers::AddonPackage>>,
    package_fetches: HashMap<String, Arc<AtomicBool>>,
    package_retry_at: HashMap<String, std::time::Instant>,
    /// In-flight and failed addon logo URLs; successes live in the image cache.
    addon_logo_requested: std::collections::HashSet<String>,
    chosen_addon: usize,
    type_defs: Vec<TypeDef>,
    chosen_type: usize,
    chosen_catalog: usize,
    chosen_genre: String,
    search: String,
    /// True while the initial set of addons is being loaded at startup.
    /// Suppresses the "All addons are disabled" hint until loading finishes.
    loading_addons: bool,
    /// Language changes during restore must not derive partial Library models.
    restoring_models: bool,
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
    /// Raw addon catalog entries; display grouping never writes into this list.
    catalog_raw: Vec<MetaPreview>,
    catalog_page: catalog::PageState,
    /// Search result set, kept separate so returning from Search restores the
    /// selected browse catalog without another request.
    search_previews: Vec<MetaPreview>,
    unified_search: unified_search::State,
    search_filters: SearchFilters,
    search_targets: Vec<SearchTarget>,
    search_generation: u64,
    /// Requests still outstanding in the current search or pagination wave.
    search_pending_requests: usize,
    search_loading_more: bool,
    search_poster_inflight: HashSet<(u64, String, String)>,
    /// Cooldown prevents failed saved artwork from flooding metadata requests.
    artwork_recovery: HashMap<(String, String, String, String), std::time::Instant>,
    /// Confirmed missing/failed URLs awaiting usable addon manifests.
    pending_artwork_recovery:
        HashMap<(String, String, String, String), Vec<posters::CatalogPosterRepair>>,
    library_poster_inflight: HashSet<(String, String, String)>,
    /// Session-only refresh state, independent of legacy header/episode caches.
    metadata_prefetch: HashMap<(String, String), MetadataPrefetch>,
    /// Aggregate background episode-inventory budget (revision, next batch).
    saved_episode_refresh_next: Option<(u64, std::time::Instant)>,
    /// Saved library items (My Library), order = insertion order.
    entries: Vec<LibraryEntry>,
    /// Typed identities in the painted Library order; actions never rederive
    /// smart filters or sorting just to resolve an already displayed card.
    library_displayed: Vec<media_identity::TitleKey>,
    /// Current image-cache settings (mirrored from the KV store).
    cache_settings: CacheSettings,
    playback_preference_edits:
        HashMap<String, Option<nova_config::playback_preferences::PreferenceValue>>,
    playback_preference_save_at: Option<std::time::Instant>,
    download_settings: DownloadSettings,
    downloads_page_query: String,
    downloads_page_filter: i32,
    downloads_page_sort_recent: bool,
    downloads_page_collapsed: HashSet<String>,
    downloads_page_refresh: downloads_page::RefreshState,
    page_artwork: page_artwork::PageArtworkState,
    release_reminders: release_reminders::State,
    /// Modal target, if open.
    modal_item: Option<ModalItem>,
    /// An untouched opening may restore its season after background alias
    /// reconciliation. Explicit navigation cancels this one-opening intent.
    detail_opening_season: Option<Arc<()>>,
    /// Streams currently displayed (already narrowed by `stream_filter`).
    streams: Vec<StreamUi>,
    /// Every stream returned for the current request, before filtering.
    stream_all: Vec<StreamUi>,
    /// A metadata refresh can rerun the same episode with new stream aliases.
    /// Older addon replies must not modify the new search's rows or pills.
    stream_generation: u64,
    stream_refresh_gate: detail::StreamRefreshGate,
    season_download: Option<season_downloads::Plan>,
    /// Visible row identities whose entrance has already been published.
    stream_revealed_ids: HashSet<String>,
    /// Display labels for pending transport identities.
    stream_owner_labels: HashMap<String, String>,
    stream_owner_order: Vec<String>,
    /// Owners whose current snapshot was served from a local stream cache.
    stream_cached_owners: HashSet<String>,
    /// Labels of the addons shown as filter pills, in installed-addon order.
    /// Pending addons are included until they answer (then dropped if they
    /// returned nothing).
    stream_addons: Vec<String>,
    /// Transport URLs still being queried (pills show a spinner until final).
    stream_pending: Vec<String>,
    /// Bounded semantic diagnostics returned by bundled providers only.
    stream_warnings: HashMap<String, Vec<(String, String)>>,
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
    /// Recent actual playback, excluding titles visible in Continue Watching.
    home_history_list: Vec<watch_history::HistoryEntry>,
    /// Explicit history view includes completed titles without populating Home.
    home_history_archive: Vec<watch_history::HistoryEntry>,
    /// Worker-prepared episode proofs for history cards and Hide actions.
    history_identities: Arc<watch_history::HistoryIdentities>,
    /// Exact episode/resume selected from History, scoped to one modal opening.
    history_selection: Option<watch_history_actions::HistorySelection>,
    /// Released episodes awaiting an explicit return, separate from active viewing.
    new_episodes_list: Vec<ContinueEntry>,
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
    /// Current episode or movie; `None` while idle.
    playback: Option<PlaybackTarget>,
    sleep_timer: sleep_timer::State,
    playback_end: playback_end::State,
    /// Resolved stream held while the user chooses whether to resume.
    pending_resume_stream: Option<PendingStream>,
    resume_prompt_choice: Option<bool>,
    /// Info hash of the torrent currently being streamed, if any. Set when a
    /// torrent resolves and opens the player; cleared when the player closes.
    /// Drives the "Connecting… ↓ rate · peers · %" status line.
    active_torrent: Option<String>,
    playback_intent: Option<playback::PlaybackIntent>,
    /// In-flight per-addon metadata refreshes (base URLs). Guards the
    /// Refresh button against double-clicks racing two manifest fetches.
    refreshing: HashSet<String>,
    /// Detail-view content choices keyed by item id (in-memory only): later
    /// visits restore the tab, season and filter without stale keyboard focus.
    detail_snapshots: HashMap<String, DetailSnapshot>,
    /// Only related-title visits form a stack; ordinary catalog opens start fresh.
    related_detail_history: Vec<detail_extras::RelatedDetailReturn>,
}

#[derive(Clone, Debug)]
enum StreamHint {
    Fixed(&'static str),
    LoadingEpisodes(usize),
    Playing(String),
    P2pUnavailable(String),
    ResolverFailure(String),
}

impl StreamHint {
    fn render(&self) -> String {
        match self {
            Self::Fixed(source) => text::tr(source).to_string(),
            Self::LoadingEpisodes(count) => text::loading_episodes(*count),
            Self::Playing(name) => text::playing(name),
            Self::P2pUnavailable(error) => text::p2p_unavailable(error),
            Self::ResolverFailure(message) => text::tr(message).to_owned(),
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
#[derive(Clone)]
struct ModalItem {
    details: detail_extras::DetailExtras,
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

/// Remember content choices for a title, without carrying keyboard focus
/// into a later visit. New Detail shells start at the top; playback return
/// uses its own explicit episode reveal instead of this snapshot.
#[derive(Clone, Debug, Default)]
struct DetailSnapshot {
    tab: i32,
    season: Option<u32>,
    filter: String,
}

// ---------------------------------------------------------------------------
// Episode playback tracking (watched + resume position + full history).
// ---------------------------------------------------------------------------

/// One episode's playback state. `EPISODE_PROGRESS_KEY` exposes a logical
/// `HashMap<String, EpisodeProgress>` keyed by [`progress_map_key`]; storage
/// persists each episode independently in the authoritative collection table.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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
    /// An explicit watch action can reaffirm an already-visible state while
    /// another provider's opposite action is still awaiting projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    manual_watch_action: Option<String>,
    /// How many times playback of this episode was opened.
    play_count: u32,
    /// Seconds since the Unix epoch of the last update.
    updated_at_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    history: Option<PlaybackHistory>,
}

/// Display metadata captured from the actual source, independent of Library membership.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct PlaybackHistory {
    type_: String,
    title: String,
    poster_url: String,
    episode_label: String,
    season: Option<u32>,
    episode: Option<u32>,
    played_at_secs: u64,
    /// Observed playing time in the current viewing run, capped at 30 seconds.
    /// None is a legacy record whose valid resume is retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    engaged_secs: Option<f64>,
    /// Actual replay of watched material, including seeking back after EOF.
    #[serde(default)]
    rewatch: bool,
}

/// In-memory target of the currently playing episode or movie.
/// Lives in [`Shared::playback`].
#[derive(Clone, Debug, Default)]
struct PlaybackTarget {
    series_id: String,
    episode_id: String,
    /// Ephemeral identity: callbacks and artwork from an older opening cannot
    /// act on a replacement stream, even when its episode ID is unchanged.
    session: u64,
    intro_skip: intro_skip::Session,
    /// Exact local artifact captured at open time, never inferred from browsing.
    intro_download: Option<crate::download::DownloadJob>,
    next_episode_dismissed: bool,
    /// One-shot automatic successor intent; captured into ordinary playback.
    force_start_over: bool,
    next_episode_id: String,
    next_episode_prefetched: Option<stream_prefetch::PlaybackKey>,
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
    /// Monotonic sample clock for qualifying real playback, never seek distance.
    history_observed_at: Option<std::time::Instant>,
    /// Immutable source metadata; browsing another title cannot change it.
    history: Option<PlaybackHistory>,
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
struct PosterJob {
    /// Exact projected slot identity; Library jobs retain their existing guard.
    display_key: Option<String>,
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

/// One registration per opening/recipe, including queued and running work.
/// Queued cancellation removes only its job ID; a running completion cannot
/// unregister another opening's request.
static EPISODE_INFLIGHT: LazyLock<Mutex<HashMap<String, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static EPISODE_JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct EpisodeThumbJob {
    context: profiles::Context,
    job_id: u64,
    request_key: String,
    url: String,
    cache_key: String,
    settings_revision: u64,
    open_token: Arc<()>,
    item_id: String,
    media_type: String,
    // Verification keeps the painted image while a source refresh runs.
    verify: bool,
}

static EPISODE_QUEUE: LazyLock<Mutex<VecDeque<EpisodeThumbJob>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));
static EPISODE_ACTIVE: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// Bridge: all UI-mutating methods run on the main thread only.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Bridge {
    app: slint::Weak<AppWindow>,
    shared: profile_session::SessionState,
    runtime: Arc<profile_session::DeviceRuntime>,
}

impl std::ops::Deref for Bridge {
    type Target = profile_session::DeviceRuntime;
    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

mod about;
mod activity_sync;
mod addon_browser;
mod addon_mgr;
mod addon_packages;
mod addon_picker;
mod android_cast;
mod bookmarks;
mod bridge;
mod cache_gc;
mod calendar;
mod casting;
mod catalog;
mod catalog_detail;
mod catalog_grouping;
#[cfg(feature = "desktop")]
mod desktop_window;
mod detail;
mod detail_extras;
mod downloads;
mod downloads_page;
mod headless_profile;
mod home;
mod home_catalog_fetch;
mod intro_skip;
mod last_stream;
mod library;
mod media_identity;
mod next_episode;
mod onboarding;
mod page_artwork;
mod party;
mod party_source;
mod persistence;
mod playback;
mod playback_actions;
mod playback_end;
mod playback_preferences;
mod playback_tools;
mod plugin_cache;
mod plugin_settings;
mod posters;
mod previously_watched;
mod profile_actions;
#[cfg(test)]
mod profile_actions_tests;
#[cfg(test)]
mod profile_management_tests;
mod profile_net;
mod profile_policy;
mod profile_session;
mod profiles;
#[cfg(all(test, feature = "desktop"))]
mod progress_identity_tests;
mod progress_projection;
mod recommendations;
mod release_reminders;
#[cfg(target_os = "android")]
mod release_reminders_android;
pub(crate) mod remote_playback;
mod run;
mod season_downloads;
mod settings;
mod settings_sync;
mod similar;
mod sleep_timer;
mod smart_collections;
mod smart_offline;
#[cfg(target_os = "android")]
mod smart_offline_android;
mod source_recovery;
mod spoiler;
mod stream_filters;
mod stream_preferences;
mod stream_prefetch;
mod subtitles;
mod sync_inspector;
mod unified_search;
#[cfg(feature = "update-check")]
mod updates;
mod viewing_journal;
mod watch_history;
mod watch_history_actions;
mod watch_queue;
pub use run::run;

mod qr;

mod backup;
mod clipboard;
pub(crate) mod documents;
mod episodes;
mod i18n;
mod io;
mod list_services;
mod streams;
mod stremio;
mod stremio_events;
mod stremio_metadata;
mod sync;
mod tracking;
mod watch_state;
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
pub(crate) use watch_state::*;

#[cfg(test)]
mod feature_rules_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod artwork_tests;

// ---------------------------------------------------------------------------
// Persistence: the installed addon list + per-addon manifest cache live in
// the KV store. Manifests are re-fetched only when an addon is installed fresh
// or the cache misses.
// ---------------------------------------------------------------------------

/// Wire format for addon entries in the KV store (and the synced record).
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct AddonStore {
    /// Bundled-source capabilities; external URL configuration remains opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<nova_providers::ProviderCapabilities>,
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
/// bucket from playback progress; other statuses pin the item to that
/// bucket regardless of progress. Set from the library card context menu.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) enum WatchStatus {
    /// Derive from progress/activity: Plan to Watch / Watching / New episodes / Completed.
    #[default]
    Auto,
    OnHold,
    Dropped,
    PlanToWatch,
}

impl WatchStatus {
    /// Planning suppresses active-viewing refreshes, but an explicit reminder
    /// still needs fresh release metadata on foreground and background paths.
    fn allows_episode_refresh(self, has_reminder: bool) -> bool {
        self == Self::Auto || (self == Self::PlanToWatch && has_reminder)
    }

    /// Badge label shown on the card while pinned (`None` when automatic).
    fn badge_label(self) -> Option<&'static str> {
        match self {
            WatchStatus::Auto => None,
            WatchStatus::OnHold => Some("On Hold"),
            WatchStatus::Dropped => Some("Dropped"),
            WatchStatus::PlanToWatch => Some("Plan to Watch"),
        }
    }
}

/// Built-in (automatic) library filters, listed before user categories in
/// the filter rail. "All" (empty filter) still shows everything.
const BUILTIN_FILTERS: &[&str] = &[
    "Watching",
    NEW_EPISODES_FILTER,
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
    /// Legacy account-list status retained for previously imported titles.
    /// A display fallback, never episode progress or permission to upload it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_tracking_status: Option<nova_tracking::ListStatus>,
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
    details: detail_extras::DetailExtras,
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

/// Resume only wakes maintenance workers; no storage I/O runs on the UI thread.
#[cfg(target_os = "android")]
pub(crate) fn note_cache_maintenance_resume() {
    nova_media::cache::gc::wake();
    plugin_cache::wake_gc_worker();
}

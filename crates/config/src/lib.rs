//! Shared settings types, runtime settings, and app paths.
//!
//! Split out so the media/cache and player crates can read settings and
//! platform paths without depending on the whole app.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

// ---------------------------------------------------------------------------
// Serde helpers
// ---------------------------------------------------------------------------

pub fn default_true() -> bool {
    true
}

/// Default LRU cache size in MB (decoded images kept in memory).
fn default_lru_mb() -> u32 {
    128
}

/// Default minimum grid columns (Discover / Library).
fn default_min_cols() -> u32 {
    2
}

/// Default playback rate (1× — normal speed).
fn default_playback_speed() -> f32 {
    1.0
}

/// Default episode behavior preserves the existing resume-history behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum EpisodeStartBehavior {
    StartOver,
    #[default]
    Resume,
    Ask,
}

/// Responsive artwork height presets shared by narrow and wide layouts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackdropSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl BackdropSize {
    pub fn from_index(index: i32) -> Self {
        match index {
            0 => Self::Small,
            2 => Self::Large,
            _ => Self::Medium,
        }
    }

    pub fn index(self) -> i32 {
        match self {
            Self::Small => 0,
            Self::Medium => 1,
            Self::Large => 2,
        }
    }
}

/// Horizontal title logo/text alignment on narrow Home and Detail layouts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeroTitleAlignment {
    Left,
    #[default]
    Center,
    Right,
}

impl HeroTitleAlignment {
    pub fn from_index(index: i32) -> Self {
        match index {
            0 => Self::Left,
            2 => Self::Right,
            _ => Self::Center,
        }
    }

    pub fn index(self) -> i32 {
        match self {
            Self::Left => 0,
            Self::Center => 1,
            Self::Right => 2,
        }
    }
}

/// Default torrent cache cap in MB (20 GiB).
fn default_torrent_max_mb() -> u64 {
    20480
}

// ---------------------------------------------------------------------------
// Torrent / P2P settings
// ---------------------------------------------------------------------------

/// Torrent streaming settings (Settings → P2P), persisted as JSON in the
/// `torrent_settings` KV entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TorrentSettings {
    /// Master switch for torrent playback. On by default.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Folder the BitTorrent session downloads into. Empty means "use the
    /// platform default" (`<app cache dir>/torrents`).
    #[serde(default)]
    pub dir: String,
    /// Maximum on-disk size of the torrent cache in megabytes. Torrents are
    /// forgotten + deleted least-recently-used first once this is exceeded.
    #[serde(default = "default_torrent_max_mb")]
    pub max_mb: u64,
    /// Download rate limit in KB/s (0 = unlimited).
    #[serde(default)]
    pub down_limit_kbps: u32,
    /// When true, a torrent's downloaded data is deleted as soon as its
    /// playback stops (no caching for replay). Overrides keep-for-replay.
    #[serde(default)]
    pub no_cache: bool,
}

impl Default for TorrentSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: String::new(),
            max_mb: default_torrent_max_mb(),
            down_limit_kbps: 0,
            no_cache: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DownloadSettings {
    #[serde(default)]
    pub auto_delete_watched: bool,
}

/// One catalog and optional genre selected for a synced Home banner or poster
/// rail. Identity uses the addon install URL plus protocol type, catalog id
/// and genre; the display name is not persisted because manifests can rename it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeCatalogSource {
    pub addon_url: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub catalog_id: String,
    /// Empty means all genres. Missing in older settings files.
    #[serde(default)]
    pub genre: String,
}

/// One built-in or addon catalog in the ordered Home landing layout.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "catalog", rename_all = "snake_case")]
pub enum HomeRowSource {
    ContinueWatching,
    NewEpisodes,
    Upcoming,
    Addon(HomeCatalogSource),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HomeRow {
    pub source: HomeRowSource,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

// ---------------------------------------------------------------------------
// Image-cache settings
// ---------------------------------------------------------------------------

/// Longest side used when downscaling before a cache write.
pub const IMAGE_DOWNSCALE_MAX: u32 = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheImageFormat {
    Jpeg,
    Webp,
}

/// Android video decoder preference (Settings → Player). The player starts at
/// the chosen decoder and steps down the chain automatically when it fails:
/// `HwPlus` → `Hw` → `Sw`, `Hw` → `Sw`, `Sw` alone. Unused on other platforms.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum AndroidHwdec {
    /// Direct MediaCodec: decoded frames stay on the GPU ("HW+").
    #[serde(rename = "hw+")]
    #[default]
    HwPlus,
    /// MediaCodec copy-back: hardware decode through system memory ("HW").
    #[serde(rename = "hw")]
    Hw,
    /// Software decode ("SW").
    #[serde(rename = "sw")]
    Sw,
}

impl AndroidHwdec {
    /// Settings segmented-control index → preference.
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => AndroidHwdec::Hw,
            2 => AndroidHwdec::Sw,
            _ => AndroidHwdec::HwPlus,
        }
    }

    /// Preference → Settings segmented-control index.
    pub fn index(self) -> i32 {
        match self {
            AndroidHwdec::HwPlus => 0,
            AndroidHwdec::Hw => 1,
            AndroidHwdec::Sw => 2,
        }
    }

    /// Short badge/menu label ("HW+" / "HW" / "SW").
    pub fn label(self) -> &'static str {
        match self {
            AndroidHwdec::HwPlus => "HW+",
            AndroidHwdec::Hw => "HW",
            AndroidHwdec::Sw => "SW",
        }
    }
}

/// Language of the user interface (Settings → Display).
///
/// English is the source language every string in `crates/ui/*.slint` is
/// written in, so it is also what `code()` returns for the fallback: the
/// bundled catalogs live in `crates/ui/translations/<code>/LC_MESSAGES/nova-ui.po`
/// and a language with no catalog (or one missing a string) simply renders the
/// English source text. Syncs by default, with an optional per-device override.
///
/// Adding a language = a variant here (plus its `ALL`/`index`/`code`/`label`
/// arms) and a catalog directory; the picker list in Settings → Display is
/// built from `ALL`, so it follows automatically.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Language {
    #[default]
    English,
    /// Croatian, `hr` — the second language, translated page by page
    /// (Settings first; strings without a catalog entry stay English).
    Croatian,
}

impl Language {
    /// Every selectable language, in Settings → Display picker order. The
    /// `Segmented` index of a language is its position here (`index()`).
    pub const ALL: &'static [Language] = &[Language::English, Language::Croatian];

    /// Settings picker index → language. Out-of-range reads as English (the
    /// fallback whenever the stored value or the UI list is out of step).
    pub fn from_index(index: i32) -> Self {
        Self::ALL
            .get(index.max(0) as usize)
            .copied()
            .unwrap_or_default()
    }

    /// Language → Settings picker index.
    pub fn index(self) -> i32 {
        Language::ALL.iter().position(|l| *l == self).unwrap_or(0) as i32
    }

    /// Locale string the UI catalogs are keyed by, matching the folder name
    /// under `crates/ui/translations/` (`"en"` = the built-in source strings).
    pub fn code(self) -> &'static str {
        match self {
            Language::English => "en",
            Language::Croatian => "hr",
        }
    }

    /// Name shown in the picker, in the language itself (never translated:
    /// you should be able to find your language when you do not read the
    /// current one).
    pub fn label(self) -> &'static str {
        match self {
            Language::English => "English",
            Language::Croatian => "Hrvatski",
        }
    }
}

/// Desktop external video app (Settings → Player → External app). Which
/// program opens the stream when the player backend is external. Unused on
/// Android, where the system resolver (`ACTION_VIEW`) picks the app.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum DesktopExternalApp {
    /// System default handler for video URLs (xdg-open on Unix, ShellExecute on Windows).
    #[default]
    #[serde(rename = "system")]
    SystemDefault,
    /// VLC (`vlc <url>`).
    #[serde(rename = "vlc")]
    Vlc,
    /// mpv (`mpv <url>`, a separate window from the in-app player).
    #[serde(rename = "mpv")]
    Mpv,
}

impl DesktopExternalApp {
    /// Settings segmented-control index → preference.
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => DesktopExternalApp::Vlc,
            2 => DesktopExternalApp::Mpv,
            _ => DesktopExternalApp::SystemDefault,
        }
    }

    /// Preference → Settings segmented-control index.
    pub fn index(self) -> i32 {
        match self {
            DesktopExternalApp::SystemDefault => 0,
            DesktopExternalApp::Vlc => 1,
            DesktopExternalApp::Mpv => 2,
        }
    }

    /// Command used for the stream URL on Unix; Windows resolves SystemDefault
    /// through ShellExecute instead.
    pub fn program(self) -> &'static str {
        match self {
            DesktopExternalApp::SystemDefault => "xdg-open",
            DesktopExternalApp::Vlc => "vlc",
            DesktopExternalApp::Mpv => "mpv",
        }
    }
}

impl CacheImageFormat {
    pub fn label(&self) -> &'static str {
        match self {
            CacheImageFormat::Jpeg => "JPEG",
            CacheImageFormat::Webp => "WebP",
        }
    }
}

/// How images should be stored in the on-disk poster cache.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CacheSettings {
    /// Master switch for on-disk image caching: when false, posters and
    /// thumbnails are never read from nor written to disk (each launch
    /// re-downloads them; the in-memory decoded cache still applies within
    /// a run). On by default.
    #[serde(default = "default_true")]
    pub cache_images: bool,
    /// Master switch: when false the cache stores raw source bytes (default).
    pub enabled: bool,
    pub format: CacheImageFormat,
    /// Encoding quality 1-100 (WebP lossy / JPEG).
    pub quality: u8,
    /// Downscale the longest side to `IMAGE_DOWNSCALE_MAX` before encoding.
    pub downscale: bool,
    /// Show episode release dates as relative ("3 days ago") vs absolute ("Mar 15, 2024").
    #[serde(default = "default_true")]
    pub date_relative: bool,
    /// Show thumbnails for unwatched episodes in the episode list. When off,
    /// episodes with no watch progress show the placeholder instead;
    /// watched and resumed episodes keep their artwork.
    #[serde(default = "default_true")]
    pub show_unwatched_thumbs: bool,
    /// Maximum decoded image cache size in megabytes.
    #[serde(default = "default_lru_mb")]
    pub lru_cache_mb: u32,
    /// Prefetch season/episode metadata for visible catalog items in the background.
    #[serde(default)]
    pub prefetch_metadata: bool,
    /// Lazily re-encode images to the configured format/quality as they are
    /// loaded, replacing the on-disk cache entry. Off by default.
    #[serde(default)]
    pub lazy_reencode: bool,
    /// One-shot migration request kept for backwards compatibility with
    /// older saved settings (previously set by a toggle, applied on save, then
    /// cleared). No longer exposed in the UI — the "Re-encode
    /// now" button runs immediately instead. Always written as false.
    #[serde(default)]
    pub rewrite_existing: bool,
    /// User-defined library category names (managed in Settings).
    #[serde(default)]
    pub categories: Vec<String>,
    /// Minimum grid columns for Discover (Settings → Display). Higher
    /// values show smaller cards — e.g. 3 across on a phone.
    #[serde(default = "default_min_cols")]
    pub discover_min_cols: u32,
    /// Show the owning addon's name before catalogs in Discover's All addons
    /// picker. Display-only: catalog identity never depends on this setting.
    #[serde(default = "default_true")]
    pub discover_catalog_addon_names: bool,
    /// Catalogs whose titles are featured and rotated at the top of Home.
    /// This selection syncs; a device can fetch it only when the matching
    /// addon is available and enabled there.
    #[serde(default)]
    pub home_catalog_sources: Vec<HomeCatalogSource>,
    /// Additional addon catalogs shown as poster rails below Upcoming on
    /// Home. Synced with settings; each device fetches from its own addons.
    #[serde(default)]
    pub home_row_sources: Vec<HomeCatalogSource>,
    /// Ordered built-in and addon catalogs. None lazily migrates legacy
    /// selections; Some(empty) intentionally means no Home catalogs.
    #[serde(default)]
    pub home_rows: Option<Vec<HomeRow>>,
    /// Show the Continue Watching row on Home. Synced with the rest of the
    /// general settings; older settings snapshots keep the existing visible
    /// behavior.
    #[serde(default = "default_true")]
    pub home_continue_enabled: bool,
    /// Show the Upcoming row on Home. Synced with the rest of the general
    /// settings; older settings snapshots keep the existing visible behavior.
    #[serde(default = "default_true")]
    pub home_upcoming_enabled: bool,
    /// Prefer episode thumbnails over series posters in Home's Continue
    /// Watching and Upcoming rows. Synced with other general settings and
    /// defaults to the existing episode-art behavior.
    #[serde(default = "default_true")]
    pub home_episode_artwork: bool,
    /// Minimum grid columns for My Library (same scheme).
    #[serde(default = "default_min_cols")]
    pub library_min_cols: u32,
    /// Android: preferred video decoder. Persisted on every platform (harmless
    /// where unused) so the same settings blob works everywhere.
    #[serde(default)]
    pub android_hwdec: AndroidHwdec,
    /// Play streams in an external video app instead of the built-in mpv
    /// player (Settings → Player). Persisted on every platform (harmless
    /// where unused) so the same settings blob works everywhere. Local-only:
    /// never synced (see `UNSYNCED_SETTINGS_FIELDS` in `src/app/sync.rs`).
    #[serde(default)]
    pub player_external: bool,
    /// Desktop: which app opens the stream when `player_external` is set
    /// (Settings → Player → External app). Unused on Android, which fires an
    /// `ACTION_VIEW` intent and lets the system resolve it. Local-only:
    /// never synced.
    #[serde(default)]
    pub desktop_external_app: DesktopExternalApp,
    /// Playback rate for the built-in player, 0.5–2.0 (Settings → Player and
    /// the player's own settings panel edit the same value). Local-only: the
    /// rate is a property of this device's listening setup, so it is never
    /// synced (see `UNSYNCED_SETTINGS_FIELDS` in `src/app/sync.rs`).
    #[serde(default = "default_playback_speed")]
    pub playback_speed: f32,
    /// How to start episodes that have resumable watch history. Local-only.
    #[serde(default)]
    pub episode_start_behavior: EpisodeStartBehavior,
    /// Android: resume on returning only if playing before backgrounding. Local-only.
    #[serde(default)]
    pub android_auto_continue: bool,
    /// Master switch for UI animations (Settings → Look and feel). When off,
    /// every transition and hover effect becomes instant.
    #[serde(default = "default_true")]
    pub animations: bool,
    /// Page / subpage slide and entrance fades.
    #[serde(default = "default_true")]
    pub anim_transitions: bool,
    /// Hover / focus feedback (button fills, card lift, toggle knob, …).
    #[serde(default = "default_true")]
    pub anim_hover: bool,
    /// Player OSD fades and control reveals.
    #[serde(default = "default_true")]
    pub anim_player: bool,
    /// Glide the navigation bar's selection marker between items.
    #[serde(default = "default_true")]
    pub anim_nav_slide: bool,
    /// Language of the user interface (Settings → Display). English is the
    /// source language, so an older settings blob without the field (or one
    /// naming a language this build does not bundle) reads as English.
    /// Syncs by default; `sync_overrides` can retain a different device language.
    #[serde(default)]
    pub language: Language,
    /// Pure black backgrounds (Settings → Theme), synced unless overridden.
    #[serde(default)]
    pub true_black: bool,
    /// Corner radius in logical pixels for media and detail cards. Synced unless overridden.
    #[serde(default = "default_card_corner_radius")]
    pub card_corner_radius: u32,
    /// Space in logical pixels between media/detail cards. Synced unless overridden.
    #[serde(default = "default_card_spacing")]
    pub card_spacing: u32,
    /// Dark top artwork fade strength in percent; zero disables it.
    #[serde(default = "default_status_bar_gradient")]
    pub status_bar_gradient: u32,
    #[serde(default)]
    pub home_backdrop_size: BackdropSize,
    #[serde(default)]
    pub detail_backdrop_size: BackdropSize,
    /// Narrow Home/Detail title alignment, synced unless overridden.
    #[serde(default)]
    pub hero_title_alignment: HeroTitleAlignment,
    /// Device-local overrides, retaining a shared baseline for unseeded fields.
    #[serde(default)]
    pub sync_overrides: std::collections::BTreeMap<String, serde_json::Value>,
}

const fn default_card_corner_radius() -> u32 {
    10
}

const fn default_card_spacing() -> u32 {
    8
}

const fn default_status_bar_gradient() -> u32 {
    80
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            cache_images: true,
            enabled: false,
            format: CacheImageFormat::Webp,
            quality: 85,
            downscale: true,
            date_relative: true,
            show_unwatched_thumbs: true,
            // Keep in step with `default_lru_mb` (the serde default used for
            // settings files written before the field existed).
            lru_cache_mb: 128,
            prefetch_metadata: false,
            lazy_reencode: false,
            rewrite_existing: false,
            categories: Vec::new(),
            discover_min_cols: 2,
            discover_catalog_addon_names: true,
            home_catalog_sources: Vec::new(),
            home_row_sources: Vec::new(),
            home_rows: None,
            home_continue_enabled: true,
            home_upcoming_enabled: true,
            home_episode_artwork: true,
            library_min_cols: 2,
            android_hwdec: AndroidHwdec::HwPlus,
            player_external: false,
            desktop_external_app: DesktopExternalApp::SystemDefault,
            playback_speed: 1.0,
            episode_start_behavior: EpisodeStartBehavior::Resume,
            android_auto_continue: false,
            animations: true,
            anim_transitions: true,
            anim_hover: true,
            anim_player: true,
            anim_nav_slide: true,
            language: Language::English,
            true_black: false,
            card_corner_radius: default_card_corner_radius(),
            card_spacing: default_card_spacing(),
            status_bar_gradient: default_status_bar_gradient(),
            home_backdrop_size: BackdropSize::default(),
            detail_backdrop_size: BackdropSize::default(),
            hero_title_alignment: HeroTitleAlignment::default(),
            sync_overrides: Default::default(),
        }
    }
}

impl CacheSettings {
    /// Preserve the old order and hidden built-ins until the first layout edit.
    pub fn effective_home_rows(&self) -> Vec<HomeRow> {
        let rows = self.home_rows.clone().unwrap_or_else(|| {
            let mut rows = vec![
                HomeRow {
                    source: HomeRowSource::ContinueWatching,
                    enabled: self.home_continue_enabled,
                },
                HomeRow {
                    source: HomeRowSource::NewEpisodes,
                    enabled: true,
                },
                HomeRow {
                    source: HomeRowSource::Upcoming,
                    enabled: self.home_upcoming_enabled,
                },
            ];
            rows.extend(self.home_row_sources.iter().cloned().map(|source| HomeRow {
                source: HomeRowSource::Addon(source),
                enabled: true,
            }));
            rows
        });
        let mut seen = std::collections::HashSet::new();
        rows.into_iter()
            .filter(|row| seen.insert(row.source.clone()))
            .collect()
    }

    pub fn home_row_enabled(&self, source: &HomeRowSource) -> bool {
        self.effective_home_rows()
            .iter()
            .any(|row| &row.source == source && row.enabled)
    }

    /// Keep legacy fields as a compatibility projection for older settings.
    pub fn set_home_rows(&mut self, rows: Vec<HomeRow>) {
        self.home_rows = Some(rows);
        let rows = self.effective_home_rows();
        self.home_continue_enabled = rows
            .iter()
            .any(|row| row.source == HomeRowSource::ContinueWatching && row.enabled);
        self.home_upcoming_enabled = rows
            .iter()
            .any(|row| row.source == HomeRowSource::Upcoming && row.enabled);
        self.home_row_sources = rows
            .iter()
            .filter_map(|row| match &row.source {
                HomeRowSource::Addon(source) => Some(source.clone()),
                _ => None,
            })
            .collect();
        self.home_rows = Some(rows);
    }

    pub fn home_addon_sources(&self, enabled_only: bool) -> Vec<HomeCatalogSource> {
        self.effective_home_rows()
            .into_iter()
            .filter_map(|row| match row.source {
                HomeRowSource::Addon(source) if !enabled_only || row.enabled => Some(source),
                _ => None,
            })
            .collect()
    }

    /// Stable key describing the active encoding config; the cache sidecar
    /// stores it so entries are lazily re-encoded when it changes.
    pub fn config_key(&self) -> String {
        let format = match self.format {
            CacheImageFormat::Jpeg => "jpeg",
            CacheImageFormat::Webp => "webp",
        };
        format!("{format}:q{}:d{}", self.quality, self.downscale as u8)
    }
}

// ---------------------------------------------------------------------------
// Playback rate (Settings → Player and the player's own settings panel)
// ---------------------------------------------------------------------------

/// Lowest selectable playback rate.
pub const PLAYBACK_SPEED_MIN: f32 = 0.5;
/// Highest selectable playback rate.
pub const PLAYBACK_SPEED_MAX: f32 = 2.0;
/// Step of the ± buttons / arrow keys: the rate stays a multiple of this.
pub const PLAYBACK_SPEED_STEP: f32 = 0.05;

/// Clamp a rate into the selectable range, leaving it otherwise untouched —
/// the slider may sit anywhere between the bounds.
pub fn clamp_playback_speed(speed: f32) -> f32 {
    if speed.is_finite() {
        speed.clamp(PLAYBACK_SPEED_MIN, PLAYBACK_SPEED_MAX)
    } else {
        1.0
    }
}

/// Clamp plus round to hundredths, so a slider drag persists (and shows) the
/// same value instead of a long float tail.
pub fn round_playback_speed(speed: f32) -> f32 {
    (clamp_playback_speed(speed) * 100.0).round() / 100.0
}

/// Clamp plus snap onto the [`PLAYBACK_SPEED_STEP`] grid — what the ± buttons
/// and arrow keys do, so repeated steps always land on exact multiples (and
/// recover the grid from a free slider value).
pub fn quantize_playback_speed(speed: f32) -> f32 {
    let steps = (clamp_playback_speed(speed) / PLAYBACK_SPEED_STEP).round();
    round_playback_speed(steps * PLAYBACK_SPEED_STEP)
}

// ---------------------------------------------------------------------------
// Runtime cache settings (read by media/player, written by the app)
// ---------------------------------------------------------------------------

static CURRENT_CACHE_SETTINGS: LazyLock<Mutex<CacheSettings>> =
    LazyLock::new(|| Mutex::new(CacheSettings::default()));

/// The active image-cache settings.
pub fn active_cache_settings() -> CacheSettings {
    CURRENT_CACHE_SETTINGS.lock().unwrap().clone()
}

/// Store the active image-cache settings (the decoded LRU budget is a
/// separate concern — see `nova-media`).
pub fn set_cache_settings(settings: CacheSettings) {
    *CURRENT_CACHE_SETTINGS.lock().unwrap() = settings;
}

// ---------------------------------------------------------------------------
// App paths
// ---------------------------------------------------------------------------

/// Directory name the app owns beneath each platform's data/cache roots and,
/// on Android, the name of the temp-dir fallback.
const APP_DIR_NAME: &str = "nova";

#[cfg(target_os = "android")]
static ANDROID_FILES_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Stash the Android files dir at startup (called once from `android_main`).
#[cfg(target_os = "android")]
pub fn set_android_files_dir(dir: PathBuf) {
    let _ = ANDROID_FILES_DIR.set(dir);
}

/// Android: app-private directory the mpv player unpacks its bundled subtitle
/// font into (`<files>/fonts`). Consumed as mpv's `sub-fonts-dir`: the vendored
/// libmpv is built without fontconfig and with libass's system-font provider
/// off, so without a font here libass can only use fonts embedded in the media
/// and text subtitles render blank.
#[cfg(target_os = "android")]
pub fn android_fonts_dir() -> PathBuf {
    app_data_dir().join("fonts")
}

/// App data dir (`$XDG_DATA_HOME/nova`, `~/.local/share/nova`, or the Android
/// files dir).
pub fn app_data_dir() -> PathBuf {
    #[cfg(target_os = "android")]
    {
        // `NOVA_DATA_DIR` overrides for device-farm testing; the temp dir is
        // a last resort (an unwritable dir degrades gracefully).
        ANDROID_FILES_DIR
            .get()
            .cloned()
            .or_else(|| std::env::var_os("NOVA_DATA_DIR").map(PathBuf::from))
            .unwrap_or_else(|| std::env::temp_dir().join(APP_DIR_NAME))
    }
    #[cfg(target_os = "windows")]
    {
        windows_app_dir(false)
    }
    #[cfg(all(not(target_os = "android"), not(target_os = "windows")))]
    {
        xdg_app_dir("XDG_DATA_HOME", ".local/share", APP_DIR_NAME)
    }
}

/// Base directory for this app under an XDG root: `$<env>/nova`, falling back
/// to `$HOME/<home_fallback>/nova`, then to the temp dir.
#[cfg(all(not(target_os = "android"), not(target_os = "windows")))]
fn xdg_app_dir(env: &str, home_fallback: &str, name: &str) -> PathBuf {
    let base = std::env::var_os(env)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(home_fallback)))
        .unwrap_or_else(std::env::temp_dir);
    base.join(name)
}

/// Windows data and cache roots use APPDATA and LOCALAPPDATA respectively,
/// with USERPROFILE\AppData fallbacks and a temp-dir last resort.
#[cfg(target_os = "windows")]
fn windows_app_dir(local: bool) -> PathBuf {
    let (env, profile_subdir) = if local {
        ("LOCALAPPDATA", "Local")
    } else {
        ("APPDATA", "Roaming")
    };
    let base = std::env::var_os(env)
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(|profile| PathBuf::from(profile).join("AppData").join(profile_subdir))
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join(APP_DIR_NAME)
}

/// App cache dir: `$XDG_CACHE_HOME/nova`, falling back to `~/.cache/nova`.
#[cfg(all(not(target_os = "android"), not(target_os = "windows")))]
pub fn app_cache_dir() -> PathBuf {
    xdg_app_dir("XDG_CACHE_HOME", ".cache", APP_DIR_NAME)
}

/// Windows cache dir: `%LOCALAPPDATA%\\nova`.
#[cfg(target_os = "windows")]
pub fn app_cache_dir() -> PathBuf {
    windows_app_dir(true)
}

/// App cache dir on Android: the sibling `cache` dir of the files dir —
/// the same location `Context.getCacheDir()` returns (`<data>/cache`,
/// next to `<data>/files`, *not* inside it). `NOVA_CACHE_DIR` overrides for
/// testing, else a temp dir.
#[cfg(target_os = "android")]
pub fn app_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("NOVA_CACHE_DIR").map(PathBuf::from) {
        return dir;
    }
    if let Some(files) = ANDROID_FILES_DIR.get() {
        return android_cache_dir_for(files);
    }
    std::env::temp_dir().join(format!("{APP_DIR_NAME}-cache"))
}

/// Map the Android files dir (`Context.getFilesDir()`) to the system cache
/// dir (`Context.getCacheDir()`). Pure helper so the sibling derivation is
/// unit-testable on host.
pub fn android_cache_dir_for(files_dir: &std::path::Path) -> PathBuf {
    if files_dir.file_name().is_some_and(|n| n == "files")
        && let Some(parent) = files_dir.parent()
    {
        return parent.join("cache");
    }
    files_dir.join("cache")
}

/// Directory holding downloaded poster images (raw bytes, keyed by URL hash).
pub fn poster_cache_dir() -> PathBuf {
    app_cache_dir().join("posters")
}

// ---------------------------------------------------------------------------
// Small utilities shared across crates
// ---------------------------------------------------------------------------

/// Log to the browser console (no-op on native; kept for call-site parity).
pub fn web_log(_msg: &str) {}

/// FNV-1a 64-bit hash, used for cache file names and stable IDs.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Milliseconds since the Unix epoch (HLC physical time).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_speed_clamps_and_rounds() {
        assert_eq!(clamp_playback_speed(0.1), PLAYBACK_SPEED_MIN);
        assert_eq!(clamp_playback_speed(9.0), PLAYBACK_SPEED_MAX);
        assert_eq!(clamp_playback_speed(1.25), 1.25);
        // A slider value keeps its place but not a float tail.
        assert_eq!(round_playback_speed(1.2345), 1.23);
        assert_eq!(round_playback_speed(0.0), PLAYBACK_SPEED_MIN);
    }

    #[test]
    fn playback_speed_steps_land_on_the_grid() {
        assert_eq!(quantize_playback_speed(1.0), 1.0);
        // ± from a free slider value first snaps back onto the grid.
        assert_eq!(quantize_playback_speed(1.234), 1.25);
        assert_eq!(quantize_playback_speed(1.0 + PLAYBACK_SPEED_STEP), 1.05);
        // Repeated steps never drift off the 0.05 grid…
        let mut speed = 1.0;
        for _ in 0..30 {
            speed = quantize_playback_speed(speed + PLAYBACK_SPEED_STEP);
        }
        assert_eq!(speed, PLAYBACK_SPEED_MAX);
        for _ in 0..10 {
            speed = quantize_playback_speed(speed - PLAYBACK_SPEED_STEP);
        }
        assert_eq!(speed, 1.5);
        // …and the range holds at both ends.
        assert_eq!(
            quantize_playback_speed(PLAYBACK_SPEED_MAX + 0.5),
            PLAYBACK_SPEED_MAX
        );
        assert_eq!(
            quantize_playback_speed(PLAYBACK_SPEED_MIN - 0.5),
            PLAYBACK_SPEED_MIN
        );
    }

    #[test]
    fn language_picker_index_round_trips() {
        for (index, language) in Language::ALL.iter().enumerate() {
            assert_eq!(language.index(), index as i32);
            assert_eq!(Language::from_index(index as i32), *language);
        }
        // Out-of-range picks (a shorter picker list than the stored index,
        // e.g. a language this build no longer bundles) read as the fallback
        // instead of panicking.
        assert_eq!(Language::from_index(-1), Language::English);
        assert_eq!(Language::from_index(99), Language::English);
        assert_eq!(Language::default(), Language::English);
        // Croatian is the second picker entry (the `Segmented` row's index 1).
        assert_eq!(Language::Croatian.index(), 1);
        assert_eq!(Language::from_index(1), Language::Croatian);
    }

    #[test]
    fn language_codes_and_labels_are_unique() {
        let mut codes: Vec<&str> = Language::ALL.iter().map(|l| l.code()).collect();
        codes.sort_unstable();
        let count = codes.len();
        codes.dedup();
        // Codes key the catalogs (`translations/<code>/`), labels the picker:
        // a duplicate would silently make one language unreachable.
        assert_eq!(codes.len(), count, "language codes must be unique");
        let mut labels: Vec<&str> = Language::ALL.iter().map(|l| l.label()).collect();
        labels.sort_unstable();
        let count = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), count, "language labels must be unique");
        // The code is the catalog folder name used by
        // `crates/ui/translations/<code>/LC_MESSAGES/nova-ui.po`.
        assert_eq!(Language::English.code(), "en");
        assert_eq!(Language::English.label(), "English");
        assert_eq!(Language::Croatian.code(), "hr");
        assert_eq!(Language::Croatian.label(), "Hrvatski");
    }

    #[test]
    fn language_defaults_to_english_for_older_settings() {
        // A settings blob saved before the field existed (the required
        // image-cache fields only) must load with English, not fail the load.
        let older = r#"{"enabled":false,"format":"webp","quality":85,"downscale":true}"#;
        let settings: CacheSettings = serde_json::from_str(older).unwrap();
        assert_eq!(settings.language, Language::English);
        assert!(!settings.android_auto_continue);
        assert_eq!(
            settings.episode_start_behavior,
            EpisodeStartBehavior::Resume
        );
        assert_eq!(CacheSettings::default().language.index(), 0);
    }

    #[test]
    fn device_override_baselines_default_empty_and_round_trip() {
        let older = r#"{"enabled":false,"format":"webp","quality":85,"downscale":true}"#;
        let mut settings: CacheSettings = serde_json::from_str(older).unwrap();
        assert!(settings.sync_overrides.is_empty());
        settings
            .sync_overrides
            .insert("quality".into(), serde_json::json!(60));
        settings.quality = 95;
        let restored: CacheSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.quality, 95);
        assert_eq!(restored.sync_overrides["quality"], 60);
    }

    #[test]
    fn card_theme_defaults_migrate_older_settings_and_preserve_square_zero_gap() {
        let older = r#"{"enabled":false,"format":"webp","quality":85,"downscale":true}"#;
        let mut settings: CacheSettings = serde_json::from_str(older).unwrap();
        assert_eq!(settings.card_corner_radius, 10);
        assert_eq!(settings.card_spacing, 8);
        settings.card_corner_radius = 0;
        settings.card_spacing = 0;
        let restored: CacheSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.card_corner_radius, 0);
        assert_eq!(restored.card_spacing, 0);
    }

    #[test]
    fn true_black_defaults_off_and_round_trips() {
        let older = r#"{"enabled":false,"format":"webp","quality":85,"downscale":true}"#;
        let mut settings: CacheSettings = serde_json::from_str(older).unwrap();
        assert!(!settings.true_black);
        settings.true_black = true;
        let restored: CacheSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(restored.true_black);
    }

    #[test]
    fn catalog_addon_names_default_on_and_round_trip() {
        let older = r#"{"enabled":false,"format":"webp","quality":85,"downscale":true}"#;
        let mut settings: CacheSettings = serde_json::from_str(older).unwrap();
        assert!(settings.discover_catalog_addon_names);
        settings.discover_catalog_addon_names = false;
        let encoded = serde_json::to_string(&settings).unwrap();
        let restored: CacheSettings = serde_json::from_str(&encoded).unwrap();
        assert!(!restored.discover_catalog_addon_names);
    }

    #[test]
    fn home_catalog_sources_default_empty_and_round_trip() {
        let older = r#"{"enabled":false,"format":"webp","quality":85,"downscale":true}"#;
        let mut settings: CacheSettings = serde_json::from_str(older).unwrap();
        assert!(settings.home_catalog_sources.is_empty());
        let legacy_source: HomeCatalogSource = serde_json::from_value(serde_json::json!({
            "addonUrl": "https://example.test/manifest.json",
            "type": "series",
            "catalogId": "trending"
        }))
        .unwrap();
        assert!(legacy_source.genre.is_empty());
        settings.home_catalog_sources.push(HomeCatalogSource {
            addon_url: "https://example.test/manifest.json".into(),
            type_: "series".into(),
            catalog_id: "trending".into(),
            genre: "Action".into(),
        });
        let encoded = serde_json::to_string(&settings).unwrap();
        let restored: CacheSettings = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored.home_catalog_sources, settings.home_catalog_sources);
        assert_eq!(
            serde_json::to_value(&restored.home_catalog_sources[0]).unwrap(),
            serde_json::json!({
                "addonUrl": "https://example.test/manifest.json",
                "type": "series",
                "catalogId": "trending",
                "genre": "Action"
            })
        );
    }
}

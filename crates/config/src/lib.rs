//! Shared settings types, runtime settings, and app paths.
//!
//! Split out so the media/cache and player crates can read settings and
//! platform paths without depending on the whole app.

pub mod catalog_grouping;
pub use catalog_grouping::{
    CatalogGroupingKey, CatalogGroupingMode, CatalogGroupingOverride, CatalogSurface,
};
pub mod playback_preferences;
pub mod smart_collections;
pub mod stream_filters;
pub use smart_collections::{SmartCollection, SmartMediaType, SmartWatchState};
pub use stream_filters::StreamFilterRules;

/// Device-local presentation preference. Never included in synced settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceMode {
    #[default]
    Automatic,
    Standard,
    Tv,
}

impl InterfaceMode {
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Standard,
            2 => Self::Tv,
            _ => Self::Automatic,
        }
    }

    pub fn index(self) -> i32 {
        match self {
            Self::Automatic => 0,
            Self::Standard => 1,
            Self::Tv => 2,
        }
    }

    pub fn is_tv(self, television: bool) -> bool {
        match self {
            Self::Automatic => television,
            Self::Standard => false,
            Self::Tv => true,
        }
    }
}

#[cfg(test)]
mod interface_mode_tests {
    use super::InterfaceMode;

    #[test]
    fn interface_mode_resolution_and_persistence() {
        for index in 0..=2 {
            let mode = InterfaceMode::from_index(index);
            assert_eq!(mode.index(), index);
            assert_eq!(
                serde_json::from_str::<InterfaceMode>(&serde_json::to_string(&mode).unwrap())
                    .unwrap(),
                mode
            );
        }
        assert_eq!(InterfaceMode::from_index(-1), InterfaceMode::Automatic);
        assert_eq!(InterfaceMode::from_index(99), InterfaceMode::Automatic);
        assert!(InterfaceMode::Automatic.is_tv(true));
        assert!(!InterfaceMode::Automatic.is_tv(false));
        assert!(!InterfaceMode::Standard.is_tv(true));
        assert!(InterfaceMode::Tv.is_tv(false));
        assert!(
            serde_json::to_value(super::CacheSettings::default())
                .unwrap()
                .get("interface_mode")
                .is_none()
        );
    }
}

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

// ---------------------------------------------------------------------------
// Serde helpers
// ---------------------------------------------------------------------------

pub fn default_keep_controls_visible_while_paused() -> bool {
    cfg!(target_os = "android")
}

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

fn default_screenshot_quality() -> u8 {
    85
}

/// Screenshot encoding is independent of artwork cache compression.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotFormat {
    #[default]
    Png,
    Jpeg,
    WebpLossless,
    WebpLossy,
}

impl ScreenshotFormat {
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Jpeg,
            2 => Self::WebpLossless,
            3 => Self::WebpLossy,
            _ => Self::Png,
        }
    }

    pub fn index(self) -> i32 {
        match self {
            Self::Png => 0,
            Self::Jpeg => 1,
            Self::WebpLossless => 2,
            Self::WebpLossy => 3,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::WebpLossless | Self::WebpLossy => "webp",
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::WebpLossless | Self::WebpLossy => "image/webp",
        }
    }
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

/// Main UI accent preset; neutral surfaces and semantic/service colors stay independent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccentColor {
    #[default]
    Purple,
    Orange,
    Blue,
    Teal,
    Rose,
    Cyan,
    Amber,
    Lime,
}

impl AccentColor {
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Orange,
            2 => Self::Blue,
            3 => Self::Teal,
            4 => Self::Rose,
            5 => Self::Cyan,
            6 => Self::Amber,
            7 => Self::Lime,
            _ => Self::Purple,
        }
    }

    pub fn index(self) -> i32 {
        match self {
            Self::Purple => 0,
            Self::Orange => 1,
            Self::Blue => 2,
            Self::Teal => 3,
            Self::Rose => 4,
            Self::Cyan => 5,
            Self::Amber => 6,
            Self::Lime => 7,
        }
    }
}

/// Shared surface palette, independent of the accent used for actions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundStyle {
    #[default]
    Default,
    AccentTinted,
    TrueBlack,
}

impl BackgroundStyle {
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Self::AccentTinted,
            2 => Self::TrueBlack,
            _ => Self::Default,
        }
    }

    pub fn index(self) -> i32 {
        match self {
            Self::Default => 0,
            Self::AccentTinted => 1,
            Self::TrueBlack => 2,
        }
    }

    pub fn from_true_black(enabled: bool) -> Self {
        if enabled {
            Self::TrueBlack
        } else {
            Self::Default
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
    WatchHistory,
    NewEpisodes,
    Upcoming,
    BecauseYouWatched,
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

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheImageFormat {
    Jpeg,
    Webp,
}

/// Global and per-title intro/recap behavior. Timings are never required to play.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntroSkipMode {
    Off,
    #[default]
    Prompt,
    Auto,
}

impl IntroSkipMode {
    pub fn index(self) -> i32 {
        match self {
            Self::Off => 0,
            Self::Prompt => 1,
            Self::Auto => 2,
        }
    }

    pub fn from_index(index: i32) -> Option<Self> {
        match index {
            0 => Some(Self::Off),
            1 => Some(Self::Prompt),
            2 => Some(Self::Auto),
            _ => None,
        }
    }
}

/// Minimum scored timing confidence for automatic skips, never manual prompts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntroSkipConfidence {
    Any,
    Low,
    #[default]
    Balanced,
    High,
    VeryHigh,
}

impl IntroSkipConfidence {
    pub fn index(self) -> i32 {
        match self {
            Self::Any => 0,
            Self::Low => 1,
            Self::Balanced => 2,
            Self::High => 3,
            Self::VeryHigh => 4,
        }
    }

    pub fn from_index(index: i32) -> Option<Self> {
        match index {
            0 => Some(Self::Any),
            1 => Some(Self::Low),
            2 => Some(Self::Balanced),
            3 => Some(Self::High),
            4 => Some(Self::VeryHigh),
            _ => None,
        }
    }

    pub fn percent(self) -> u8 {
        match self {
            Self::Any => 0,
            Self::Low => 25,
            Self::Balanced => 50,
            Self::High => 80,
            Self::VeryHigh => 95,
        }
    }
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

/// Retained media-segment budget for a casting packet-copy presentation.
/// Processing buffers and the rest of the app use additional memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CastingRemuxBuffer {
    #[serde(rename = "32_mib")]
    Mib32,
    #[serde(rename = "64_mib")]
    Mib64,
    #[serde(rename = "128_mib")]
    Mib128,
    #[serde(rename = "256_mib")]
    Mib256,
}

impl Default for CastingRemuxBuffer {
    fn default() -> Self {
        if cfg!(target_os = "android") {
            Self::Mib32
        } else {
            Self::Mib64
        }
    }
}

impl CastingRemuxBuffer {
    pub const fn mib(self) -> u32 {
        match self {
            Self::Mib32 => 32,
            Self::Mib64 => 64,
            Self::Mib128 => 128,
            Self::Mib256 => 256,
        }
    }

    pub const fn bytes(self) -> usize {
        self.mib() as usize * 1024 * 1024
    }

    pub const fn index(self) -> i32 {
        match self {
            Self::Mib32 => 0,
            Self::Mib64 => 1,
            Self::Mib128 => 2,
            Self::Mib256 => 3,
        }
    }

    pub const fn from_index(index: i32) -> Option<Self> {
        match index {
            0 => Some(Self::Mib32),
            1 => Some(Self::Mib64),
            2 => Some(Self::Mib128),
            3 => Some(Self::Mib256),
            _ => None,
        }
    }
}

/// Device-local desktop decoder preference. Windows retains copy-back by
/// default; Linux starts with direct hardware decoding and normal fallbacks.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum DesktopHwdec {
    #[serde(rename = "hw+")]
    HwPlus,
    #[serde(rename = "hw")]
    Hw,
    #[serde(rename = "sw")]
    Sw,
}

impl Default for DesktopHwdec {
    fn default() -> Self {
        if cfg!(target_os = "windows") {
            Self::Hw
        } else {
            Self::HwPlus
        }
    }
}

impl DesktopHwdec {
    /// Resolve preferences saved before a platform's decoder support changed.
    pub fn supported(self) -> Self {
        if cfg!(target_os = "windows") && self == Self::HwPlus {
            Self::Hw
        } else {
            self
        }
    }

    pub fn from_index(index: i32) -> Self {
        match index {
            0 => Self::HwPlus,
            1 => Self::Hw,
            2 => Self::Sw,
            _ => Self::default(),
        }
    }

    pub fn index(self) -> i32 {
        match self {
            Self::HwPlus => 0,
            Self::Hw => 1,
            Self::Sw => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::HwPlus => "HW+",
            Self::Hw => "HW",
            Self::Sw => "SW",
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

/// Retained desktop external video app preference for a future rework. Unused on
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

// Keep a single field declaration for serialization and the migration decoder.
// The remote derive constructs CacheSettings after legacy JSON has been normalized.
const fn default_image_disk_cache_limit() -> Option<u32> {
    Some(1024)
}

macro_rules! cache_settings_type {
    ($($fields:tt)*) => {
        #[derive(Clone, Debug, PartialEq, Serialize)]
        pub struct CacheSettings { $($fields)* }
        #[derive(Deserialize)]
        #[serde(remote = "CacheSettings")]
        struct CacheSettingsWire { $($fields)* }
    };
}
cache_settings_type! {
    /// Display-only addon catalog grouping; older and new profiles keep original cards.
    #[serde(default)]
    pub catalog_grouping_default: CatalogGroupingMode,
    /// One profile settings field, keyed by exact installation/catalog/surface.
    #[serde(default, deserialize_with = "catalog_grouping::deserialize_overrides")]
    pub catalog_grouping_overrides: Vec<CatalogGroupingOverride>,
    /// Opt-in parent-only Series inference; never establishes episode identity.
    #[serde(default)]
    pub catalog_grouping_infer_series: bool,
    /// Profile-owned source rules, shared unless this device overrides the field.
    #[serde(default)]
    pub stream_filters: StreamFilterRules,
    /// Dynamic Library collections with the same field-level override policy.
    #[serde(default, deserialize_with = "smart_collections::deserialize_collections")]
    pub smart_collections: Vec<SmartCollection>,
    /// TMDB v3 API key for title recommendations; profile-owned and synced.
    #[serde(default)]
    pub tmdb_api_key: String,
    /// Master switch for on-disk image caching: when false, posters and
    /// thumbnails are never read from nor written to disk (each launch
    /// re-downloads them; the in-memory decoded cache still applies within
    /// a run). On by default.
    #[serde(default = "default_true")]
    pub cache_images: bool,
    /// Synced disk artwork budget in MiB; None keeps an unlimited cache.
    #[serde(default = "default_image_disk_cache_limit")]
    pub image_disk_cache_limit_mb: Option<u32>,
    /// Encode processed variants lossily when enabled; otherwise resizing is lossless.
    pub enabled: bool,
    pub format: CacheImageFormat,
    /// Encoding quality 1-100 (WebP lossy / JPEG).
    pub quality: u8,
    /// Longest side of Detail posters; None preserves original dimensions.
    pub detail_poster_max_side: Option<u32>,
    /// Longest side of Detail banners; None preserves original dimensions.
    pub detail_backdrop_max_side: Option<u32>,
    /// Longest side of episode-list and season-picker images.
    pub episode_thumbnail_max_side: Option<u32>,
    /// Longest side of the episode image on the player loading screen.
    pub player_episode_backdrop_max_side: Option<u32>,
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
    /// selections; after recommendation migration, Some(empty) means no rows.
    #[serde(default)]
    pub home_rows: Option<Vec<HomeRow>>,
    /// Older layouts receive the recommendation catalog once. Once the user
    /// edits the layout, absence means they intentionally removed this row.
    #[serde(default)]
    pub home_recommendations_migrated: bool,
    /// Permanently dismissed recommendation identities, encoded as JSON
    /// (media type, title id) pairs. Synced with this profile's settings.
    #[serde(default)]
    pub home_recommendations_hidden: std::collections::BTreeSet<String>,
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
    /// defaults to series posters when no preference has been saved.
    #[serde(default)]
    pub home_episode_artwork: bool,
    /// Show paired-device playback on Home; synced with general settings.
    #[serde(default = "default_true")]
    pub home_devices_enabled: bool,
    /// Allow paired devices to inspect and control playback here. Device-local; never synced.
    #[serde(default = "default_true")]
    pub remote_control_enabled: bool,
    /// Ask once for unrated tracker releases completed locally. Synced with general settings.
    #[serde(default = "default_true")]
    pub completion_rating_prompts_enabled: bool,
    /// Minimum grid columns for My Library (same scheme).
    #[serde(default = "default_min_cols")]
    pub library_min_cols: u32,
    /// Android: preferred video decoder. Persisted on every platform (harmless
    /// where unused) so the same settings blob works everywhere.
    #[serde(default)]
    pub android_hwdec: AndroidHwdec,
    /// Windows/Linux decoder preference; device-local and ignored elsewhere.
    #[serde(default)]
    pub desktop_hwdec: DesktopHwdec,
    /// Android: play streams in an external video app instead of the built-in mpv
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
    /// Synced global playback defaults: comma-separated languages in priority order.
    /// Existing single-language values remain valid. Empty means Automatic;
    /// subtitle language "off" disables subtitles. OSD edits override per title.
    #[serde(default)]
    pub default_audio_language: String,
    #[serde(default)]
    pub default_subtitle_language: String,
    #[serde(default = "default_playback_speed")]
    pub playback_speed: f32,
    #[serde(default)]
    pub subtitle_style: playback_preferences::SubtitleStyle,
    /// Synced screenshot encoding; old settings retain lossless PNG.
    #[serde(default)]
    pub screenshot_format: ScreenshotFormat,
    #[serde(default = "default_screenshot_quality")]
    pub screenshot_jpeg_quality: u8,
    #[serde(default = "default_screenshot_quality")]
    pub screenshot_webp_quality: u8,
    /// Save under Pictures/Nova/<show> instead of Pictures/Nova.
    #[serde(default)]
    pub screenshot_show_folder: bool,
    /// How to start episodes that have resumable watch history. Local-only.
    #[serde(default)]
    pub episode_start_behavior: EpisodeStartBehavior,
    /// Android: resume on returning only if playing before backgrounding. Local-only.
    #[serde(default)]
    pub android_auto_continue: bool,
    /// Android: allow Home to enter system picture in picture. Device-local.
    #[serde(default = "default_true")]
    pub android_pip_enabled: bool,
    /// Android: request landscape while the built-in player is open. Device-local.
    #[serde(default = "default_true")]
    pub android_auto_rotate: bool,
    /// Warm the next episode source list in memory near the end of playback.
    #[serde(default = "default_true")]
    pub prefetch_next_episode_streams: bool,
    /// Show chapter dividers and the current chapter shortcut on the timeline.
    #[serde(default = "default_true")]
    pub show_chapters: bool,
    /// Keep paused controls visible until explicitly dismissed. Preserve the
    /// existing Android (on) and desktop (off) behavior for older settings.
    #[serde(default = "default_keep_controls_visible_while_paused")]
    pub keep_controls_visible_while_paused: bool,
    /// Show routine initial stream-loading messages. Errors remain visible.
    #[serde(default = "default_true")]
    pub show_stream_loading_text: bool,
    /// Retained casting remux media segments; processing uses additional RAM.
    #[serde(default)]
    pub casting_remux_buffer: CastingRemuxBuffer,
    /// Offer an eight-second automatic successor preview at native EOF.
    #[serde(default)]
    pub auto_next_episode: bool,
    /// Intro/recap timing behavior; explicit per-title choices override this.
    #[serde(default)]
    pub intro_skip_mode: IntroSkipMode,
    #[serde(default)]
    pub intro_skip_confidence: IntroSkipConfidence,
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
    /// Background surfaces (Settings → Theme), synced unless overridden.
    #[serde(default)]
    pub background_style: BackgroundStyle,
    /// Main interface accent (Settings → Theme), synced unless overridden.
    #[serde(default)]
    pub accent_color: AccentColor,
    /// Corner radius in logical pixels for shared buttons and controls.
    /// Synced unless overridden; individual controls cap it at half their size.
    #[serde(default = "default_button_corner_radius")]
    pub button_corner_radius: u32,
    /// Corner radius in logical pixels for media and detail cards. Synced unless overridden.
    #[serde(default = "default_card_corner_radius")]
    pub card_corner_radius: u32,
    /// Space in logical pixels between media/detail cards. Synced unless overridden.
    #[serde(default = "default_card_spacing")]
    pub card_spacing: u32,
    /// Dark top artwork fade strength in percent; zero disables it.
    #[serde(default = "default_status_bar_gradient")]
    pub status_bar_gradient: u32,
    /// Overlay the phone navigation bar on content with a dark gradient.
    #[serde(default = "default_true")]
    pub transparent_navbar: bool,
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

pub const IMAGE_RESOLUTION_PRESETS: &[u32] = &[240, 360, 480, 720, 960, 1024, 1280, 1920, 2560];

impl<'de> Deserialize<'de> for CacheSettings {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = serde_json::Value::deserialize(deserializer)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| serde::de::Error::custom("settings must be an object"))?;
        if !object.contains_key("background_style") {
            let black = object
                .get("true_black")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            object.insert(
                "background_style".into(),
                serde_json::json!(BackgroundStyle::from_true_black(black)),
            );
        }
        if let Some(overrides) = object
            .get_mut("sync_overrides")
            .and_then(serde_json::Value::as_object_mut)
            && let Some(legacy) = overrides
                .remove("true_black")
                .and_then(|value| value.as_bool())
        {
            overrides
                .entry("background_style")
                .or_insert_with(|| serde_json::json!(BackgroundStyle::from_true_black(legacy)));
        }
        let legacy_player =
            if object.get("downscale").and_then(serde_json::Value::as_bool) == Some(true) {
                serde_json::json!(1024)
            } else {
                serde_json::Value::Null
            };
        for (key, fallback) in [
            ("detail_poster_max_side", serde_json::json!(480)),
            ("detail_backdrop_max_side", serde_json::json!(960)),
            ("episode_thumbnail_max_side", serde_json::json!(360)),
            ("player_episode_backdrop_max_side", legacy_player),
        ] {
            let setting = object.entry(key).or_insert(fallback);
            if !setting.is_null()
                && !setting.as_u64().is_some_and(|n| {
                    IMAGE_RESOLUTION_PRESETS
                        .iter()
                        .any(|preset| u64::from(*preset) == n)
                })
            {
                return Err(serde::de::Error::custom(format!(
                    "invalid image resolution for {key}"
                )));
            }
        }
        CacheSettingsWire::deserialize(value).map_err(serde::de::Error::custom)
    }
}

const fn default_card_corner_radius() -> u32 {
    10
}

const fn default_button_corner_radius() -> u32 {
    8
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
            catalog_grouping_default: CatalogGroupingMode::Original,
            catalog_grouping_overrides: Vec::new(),
            catalog_grouping_infer_series: false,
            stream_filters: StreamFilterRules::default(),
            smart_collections: Vec::new(),
            tmdb_api_key: String::new(),
            cache_images: true,
            image_disk_cache_limit_mb: default_image_disk_cache_limit(),
            enabled: false,
            format: CacheImageFormat::Webp,
            quality: 85,
            detail_poster_max_side: Some(480),
            detail_backdrop_max_side: Some(960),
            episode_thumbnail_max_side: Some(360),
            player_episode_backdrop_max_side: None,
            date_relative: true,
            show_unwatched_thumbs: true,
            // Keep in step with `default_lru_mb` (the serde default used for
            // settings files written before the field existed).
            lru_cache_mb: 128,
            prefetch_metadata: false,
            categories: Vec::new(),
            discover_min_cols: 2,
            discover_catalog_addon_names: true,
            home_catalog_sources: Vec::new(),
            home_row_sources: Vec::new(),
            home_rows: None,
            home_recommendations_migrated: false,
            home_recommendations_hidden: Default::default(),
            home_continue_enabled: true,
            home_upcoming_enabled: true,
            home_episode_artwork: false,
            home_devices_enabled: true,
            remote_control_enabled: true,
            completion_rating_prompts_enabled: true,
            library_min_cols: 2,
            android_hwdec: AndroidHwdec::HwPlus,
            desktop_hwdec: DesktopHwdec::default(),
            player_external: false,
            desktop_external_app: DesktopExternalApp::SystemDefault,
            playback_speed: 1.0,
            subtitle_style: Default::default(),
            screenshot_format: ScreenshotFormat::Png,
            screenshot_jpeg_quality: default_screenshot_quality(),
            screenshot_webp_quality: default_screenshot_quality(),
            screenshot_show_folder: false,
            default_audio_language: String::new(),
            default_subtitle_language: String::new(),
            episode_start_behavior: EpisodeStartBehavior::Resume,
            android_auto_continue: false,
            android_pip_enabled: true,
            android_auto_rotate: true,
            intro_skip_mode: IntroSkipMode::Prompt,
            intro_skip_confidence: IntroSkipConfidence::default(),
            prefetch_next_episode_streams: true,
            show_chapters: true,
            keep_controls_visible_while_paused: default_keep_controls_visible_while_paused(),
            show_stream_loading_text: true,
            casting_remux_buffer: CastingRemuxBuffer::default(),
            auto_next_episode: false,
            animations: true,
            anim_transitions: true,
            anim_hover: true,
            anim_player: true,
            anim_nav_slide: true,
            language: Language::English,
            background_style: BackgroundStyle::default(),
            accent_color: AccentColor::default(),
            button_corner_radius: default_button_corner_radius(),
            card_corner_radius: default_card_corner_radius(),
            card_spacing: default_card_spacing(),
            status_bar_gradient: default_status_bar_gradient(),
            transparent_navbar: true,
            home_backdrop_size: BackdropSize::default(),
            detail_backdrop_size: BackdropSize::default(),
            hero_title_alignment: HeroTitleAlignment::default(),
            sync_overrides: Default::default(),
        }
    }
}

impl CacheSettings {
    /// Desktop external playback is suspended until its progress tracking is
    /// reworked. Keep the saved device preference for Android and future use.
    pub fn uses_external_player(&self) -> bool {
        cfg!(target_os = "android") && self.player_external
    }

    /// Add default built-ins to legacy layouts and migrate recommendations once.
    pub fn effective_home_rows(&self) -> Vec<HomeRow> {
        let mut rows = self.home_rows.clone().unwrap_or_else(|| {
            let mut rows = vec![
                HomeRow {
                    source: HomeRowSource::ContinueWatching,
                    enabled: self.home_continue_enabled,
                },
                HomeRow {
                    source: HomeRowSource::WatchHistory,
                    enabled: true,
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
        if !self.home_recommendations_migrated
            && !rows
                .iter()
                .any(|row| row.source == HomeRowSource::BecauseYouWatched)
        {
            rows.push(HomeRow {
                source: HomeRowSource::BecauseYouWatched,
                enabled: true,
            });
        }
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
        // The input is the user's complete ordered layout, so do not re-add
        // recommendations when that edit deliberately removes the catalog.
        self.home_recommendations_migrated = true;
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

    /// Encoding summary for compatibility with callers displaying the configuration.
    pub fn config_key(&self) -> String {
        let format = match self.format {
            CacheImageFormat::Jpeg => "jpeg",
            CacheImageFormat::Webp => "webp",
        };
        format!("{format}:q{}", self.quality)
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

/// Cheap hit-path policy check without cloning the full settings snapshot.
pub fn disk_image_cache_enabled() -> bool {
    CURRENT_CACHE_SETTINGS.lock().unwrap().cache_images
}

/// The active image-cache settings.
pub fn active_cache_settings() -> CacheSettings {
    image_settings_snapshot().0
}

pub fn image_settings_snapshot() -> (CacheSettings, u64) {
    let settings = CURRENT_CACHE_SETTINGS.lock().unwrap();
    (settings.clone(), image_settings_revision())
}

/// Store the active image-cache settings (the decoded LRU budget is a
/// separate concern — see `nova-media`).
static IMAGE_SETTINGS_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Revision used to discard image completions started before a recipe change.
pub fn image_settings_revision() -> u64 {
    IMAGE_SETTINGS_REVISION.load(std::sync::atomic::Ordering::Acquire)
}

pub fn set_cache_settings(settings: CacheSettings) {
    let mut current = CURRENT_CACHE_SETTINGS.lock().unwrap();
    if current.cache_images != settings.cache_images
        || current.enabled != settings.enabled
        || current.format != settings.format
        || current.quality != settings.quality
        || current.detail_poster_max_side != settings.detail_poster_max_side
        || current.detail_backdrop_max_side != settings.detail_backdrop_max_side
        || current.episode_thumbnail_max_side != settings.episode_thumbnail_max_side
        || current.player_episode_backdrop_max_side != settings.player_episode_backdrop_max_side
    {
        IMAGE_SETTINGS_REVISION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
    *current = settings;
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
    xdg_app_dir_from(
        std::env::var_os(env),
        std::env::var_os("HOME"),
        home_fallback,
        name,
    )
}

#[cfg(all(not(target_os = "android"), not(target_os = "windows")))]
fn xdg_app_dir_from(
    root: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    home_fallback: &str,
    name: &str,
) -> PathBuf {
    let base = root
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(home_fallback))
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join(name)
}

#[test]
#[cfg(all(not(target_os = "android"), not(target_os = "windows")))]
fn xdg_roots_must_be_absolute() {
    for fallback in [".local/share", ".cache"] {
        for root in [None, Some(""), Some("relative/path")] {
            assert_eq!(
                xdg_app_dir_from(
                    root.map(Into::into),
                    Some("/home/test".into()),
                    fallback,
                    "nova"
                ),
                PathBuf::from("/home/test").join(fallback).join("nova")
            );
        }
        assert_eq!(
            xdg_app_dir_from(
                Some("/custom".into()),
                Some("/home/test".into()),
                fallback,
                "nova"
            ),
            PathBuf::from("/custom/nova")
        );
        assert_eq!(
            xdg_app_dir_from(None, Some("".into()), fallback, "nova"),
            std::env::temp_dir().join("nova")
        );
    }
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
    fn saved_external_preference_only_applies_on_android() {
        let settings = CacheSettings {
            player_external: true,
            desktop_external_app: DesktopExternalApp::Vlc,
            ..CacheSettings::default()
        };
        let encoded = serde_json::to_string(&settings).unwrap();
        let restored: CacheSettings = serde_json::from_str(&encoded).unwrap();
        assert!(restored.player_external);
        assert_eq!(restored.desktop_external_app, DesktopExternalApp::Vlc);
        assert_eq!(restored.uses_external_player(), cfg!(target_os = "android"));
        assert!(!CacheSettings::default().uses_external_player());
    }

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
    fn home_catalog_defaults_enable_all_builtins_in_display_order() {
        let expected: Vec<HomeRow> = [
            HomeRowSource::ContinueWatching,
            HomeRowSource::WatchHistory,
            HomeRowSource::NewEpisodes,
            HomeRowSource::Upcoming,
            HomeRowSource::BecauseYouWatched,
        ]
        .into_iter()
        .map(|source| HomeRow {
            source,
            enabled: true,
        })
        .collect();

        let fresh = CacheSettings::default();
        let restored: CacheSettings =
            serde_json::from_str(&serde_json::to_string(&fresh).unwrap()).unwrap();
        // Settings written before Home preferences existed must agree with
        // fresh installs, rather than silently disabling a built-in row.
        let legacy: CacheSettings =
            serde_json::from_str(r#"{"enabled":false,"format":"webp","quality":85}"#).unwrap();
        for settings in [fresh, restored, legacy] {
            assert_eq!(settings.effective_home_rows(), expected);
            assert!(settings.home_continue_enabled);
            assert!(settings.home_upcoming_enabled);
        }
    }

    #[test]
    fn watch_history_defaults_after_continue_without_changing_custom_home_rows() {
        let legacy: CacheSettings = serde_json::from_str(
            r#"{"enabled":false,"format":"webp","quality":85,"home_continue_enabled":false}"#,
        )
        .unwrap();
        let rows = legacy.effective_home_rows();
        assert_eq!(rows[0].source, HomeRowSource::ContinueWatching);
        assert!(!rows[0].enabled);
        assert_eq!(rows[1].source, HomeRowSource::WatchHistory);
        assert!(rows[1].enabled);
        assert_eq!(rows[2].source, HomeRowSource::NewEpisodes);
        assert_eq!(rows[3].source, HomeRowSource::Upcoming);

        let mut custom = legacy;
        custom.set_home_rows(vec![HomeRow {
            source: HomeRowSource::Upcoming,
            enabled: true,
        }]);
        assert_eq!(custom.effective_home_rows().len(), 1);
        assert!(!custom.home_row_enabled(&HomeRowSource::WatchHistory));
        custom.set_home_rows(Vec::new());
        assert!(custom.effective_home_rows().is_empty());
    }

    #[test]
    fn watch_history_layout_choice_round_trips_and_preserves_visibility() {
        let mut settings = CacheSettings::default();
        settings.set_home_rows(vec![HomeRow {
            source: HomeRowSource::WatchHistory,
            enabled: false,
        }]);
        let encoded = serde_json::to_string(&settings).unwrap();
        assert!(encoded.contains(r#""kind":"watch_history""#));
        let restored: CacheSettings = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            restored.effective_home_rows(),
            settings.effective_home_rows()
        );
        assert!(!restored.home_row_enabled(&HomeRowSource::WatchHistory));
        assert!(restored.home_row_sources.is_empty());
    }

    #[test]
    fn recommendations_append_to_existing_layout_once_and_preserve_user_edits() {
        let custom = HomeRow {
            source: HomeRowSource::Addon(HomeCatalogSource {
                addon_url: "https://example.test/manifest.json".into(),
                type_: "movie".into(),
                catalog_id: "popular".into(),
                genre: String::new(),
            }),
            enabled: false,
        };
        // An existing explicit layout is migrated without reordering its rows
        // or accidentally enabling a catalog the user already disabled.
        let mut settings = CacheSettings {
            home_rows: Some(vec![custom.clone()]),
            ..Default::default()
        };
        let migrated = settings.effective_home_rows();
        assert_eq!(migrated[0], custom);
        assert_eq!(migrated.len(), 2);
        assert_eq!(migrated[1].source, HomeRowSource::BecauseYouWatched);
        assert!(migrated[1].enabled);
        assert_eq!(settings.effective_home_rows(), migrated);

        settings.home_rows = Some(vec![HomeRow {
            source: HomeRowSource::BecauseYouWatched,
            enabled: false,
        }]);
        assert_eq!(settings.effective_home_rows().len(), 1);
        assert!(!settings.home_row_enabled(&HomeRowSource::BecauseYouWatched));

        settings.set_home_rows(vec![HomeRow {
            source: HomeRowSource::BecauseYouWatched,
            enabled: false,
        }]);
        let restored: CacheSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(restored.home_recommendations_migrated);
        assert!(!restored.home_row_enabled(&HomeRowSource::BecauseYouWatched));
        assert_eq!(restored.effective_home_rows().len(), 1);

        settings.set_home_rows(vec![custom.clone()]);
        let restored: CacheSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.effective_home_rows(), vec![custom]);
        settings.set_home_rows(Vec::new());
        assert!(settings.effective_home_rows().is_empty());
    }

    #[test]
    fn recommendations_append_after_legacy_addons_and_hidden_identities_round_trip() {
        let mut settings = CacheSettings::default();
        settings.home_row_sources.push(HomeCatalogSource {
            addon_url: "https://example.test/manifest.json".into(),
            type_: "movie".into(),
            catalog_id: "popular".into(),
            genre: String::new(),
        });
        let rows = settings.effective_home_rows();
        assert!(matches!(rows[4].source, HomeRowSource::Addon(_)));
        assert_eq!(rows[5].source, HomeRowSource::BecauseYouWatched);
        let identity = serde_json::to_string(&("series", "native:id:1")).unwrap();
        settings
            .home_recommendations_hidden
            .insert(identity.clone());
        let restored: CacheSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.home_recommendations_hidden.len(), 1);
        assert!(restored.home_recommendations_hidden.contains(&identity));
    }

    #[test]
    fn language_defaults_to_english_for_older_settings() {
        // A settings blob saved before the field existed (the required
        // image-cache fields only) must load with English, not fail the load.
        let older = r#"{"enabled":false,"format":"webp","quality":85,"downscale":true}"#;
        let settings: CacheSettings = serde_json::from_str(older).unwrap();
        assert_eq!(settings.language, Language::English);
        assert_eq!(settings.desktop_hwdec, DesktopHwdec::default());
        assert!(!settings.android_auto_continue);
        assert!(settings.android_pip_enabled);
        assert!(settings.android_auto_rotate);
        assert!(CacheSettings::default().android_pip_enabled);
        let disabled = CacheSettings {
            android_pip_enabled: false,
            android_auto_rotate: false,
            show_chapters: false,
            ..Default::default()
        };
        let restored: CacheSettings =
            serde_json::from_str(&serde_json::to_string(&disabled).unwrap()).unwrap();
        assert!(!restored.android_pip_enabled);
        assert!(!restored.android_auto_rotate);
        assert!(!restored.show_chapters);
        assert_eq!(settings.intro_skip_mode, IntroSkipMode::Prompt);
        assert_eq!(settings.intro_skip_confidence.percent(), 50);
        assert!(settings.prefetch_next_episode_streams);
        assert!(settings.show_chapters);
        assert!(!settings.auto_next_episode);
        assert_eq!(
            settings.episode_start_behavior,
            EpisodeStartBehavior::Resume
        );
        assert_eq!(CacheSettings::default().language.index(), 0);
    }

    #[test]
    fn casting_remux_buffer_validates_choices_and_preserves_legacy_platform_defaults() {
        let older = r#"{"enabled":false,"format":"webp","quality":85}"#;
        let older: CacheSettings = serde_json::from_str(older).unwrap();
        assert_eq!(older.casting_remux_buffer, CastingRemuxBuffer::default());
        assert_eq!(
            older.casting_remux_buffer.mib(),
            if cfg!(target_os = "android") { 32 } else { 64 }
        );
        for (index, mib) in [32, 64, 128, 256].into_iter().enumerate() {
            let budget = CastingRemuxBuffer::from_index(index as i32).unwrap();
            assert_eq!(budget.index(), index as i32);
            assert_eq!(budget.mib(), mib);
            assert_eq!(budget.bytes(), mib as usize * 1024 * 1024);
            let settings = CacheSettings {
                casting_remux_buffer: budget,
                ..Default::default()
            };
            let restored: CacheSettings =
                serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert_eq!(restored.casting_remux_buffer, budget);
            assert_eq!(
                serde_json::to_value(budget).unwrap(),
                serde_json::json!(format!("{mib}_mib"))
            );
        }
        for invalid in [-1, 4, i32::MAX] {
            assert!(CastingRemuxBuffer::from_index(invalid).is_none());
        }
        for invalid in [
            serde_json::json!(0),
            serde_json::json!("512_mib"),
            serde_json::Value::Null,
        ] {
            assert!(serde_json::from_value::<CastingRemuxBuffer>(invalid).is_err());
        }
    }

    #[test]
    fn paused_controls_default_matches_platform_and_explicit_values_round_trip() {
        let older = r#"{"enabled":false,"format":"webp","quality":85}"#;
        let settings: CacheSettings = serde_json::from_str(older).unwrap();
        assert_eq!(
            settings.keep_controls_visible_while_paused,
            cfg!(target_os = "android")
        );
        assert_eq!(
            CacheSettings::default().keep_controls_visible_while_paused,
            settings.keep_controls_visible_while_paused
        );
        for enabled in [false, true] {
            let settings = CacheSettings {
                keep_controls_visible_while_paused: enabled,
                ..Default::default()
            };
            let restored: CacheSettings =
                serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert_eq!(restored.keep_controls_visible_while_paused, enabled);
        }
    }

    #[test]
    fn loading_status_text_defaults_on_and_explicit_choices_round_trip() {
        let legacy: CacheSettings =
            serde_json::from_str(r#"{"enabled":false,"format":"webp","quality":85}"#).unwrap();
        assert!(legacy.show_stream_loading_text);
        for show in [false, true] {
            let settings = CacheSettings {
                show_stream_loading_text: show,
                ..Default::default()
            };
            let restored: CacheSettings =
                serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert_eq!(restored.show_stream_loading_text, show);
        }
    }

    #[test]
    fn intro_skip_confidence_presets_round_trip_and_default_to_fifty_percent() {
        assert_eq!(CacheSettings::default().intro_skip_confidence.percent(), 50);
        for (index, percent) in [0, 25, 50, 80, 95].into_iter().enumerate() {
            let preset = IntroSkipConfidence::from_index(index as i32).unwrap();
            assert_eq!(preset.index(), index as i32);
            assert_eq!(preset.percent(), percent);
            let settings = CacheSettings {
                intro_skip_confidence: preset,
                ..Default::default()
            };
            let decoded: CacheSettings =
                serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert_eq!(decoded.intro_skip_confidence, preset);
        }
        assert!(IntroSkipConfidence::from_index(-1).is_none());
        assert!(IntroSkipConfidence::from_index(5).is_none());
    }

    #[test]
    fn desktop_decoder_choices_round_trip_with_platform_default() {
        assert_eq!(
            DesktopHwdec::HwPlus.supported(),
            if cfg!(target_os = "windows") {
                DesktopHwdec::Hw
            } else {
                DesktopHwdec::HwPlus
            }
        );
        assert_eq!(DesktopHwdec::Hw.supported(), DesktopHwdec::Hw);
        assert_eq!(DesktopHwdec::Sw.supported(), DesktopHwdec::Sw);
        assert_eq!(
            DesktopHwdec::default().index(),
            if cfg!(target_os = "windows") { 1 } else { 0 }
        );
        for index in 0..=2 {
            let settings = CacheSettings {
                desktop_hwdec: DesktopHwdec::from_index(index),
                ..Default::default()
            };
            let restored: CacheSettings =
                serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert_eq!(restored.desktop_hwdec.index(), index);
        }
        assert_eq!(DesktopHwdec::from_index(-1), DesktopHwdec::default());
        assert_eq!(DesktopHwdec::from_index(99), DesktopHwdec::default());
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
        assert!(settings.transparent_navbar);
        assert!(CacheSettings::default().transparent_navbar);
        settings.card_corner_radius = 0;
        settings.card_spacing = 0;
        settings.transparent_navbar = false;
        let restored: CacheSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.card_corner_radius, 0);
        assert_eq!(restored.card_spacing, 0);
        assert!(!restored.transparent_navbar);
    }

    #[test]
    fn button_radius_defaults_for_older_settings_and_round_trips() {
        let older = r#"{"enabled":false,"format":"webp","quality":85,"downscale":true}"#;
        let mut settings: CacheSettings = serde_json::from_str(older).unwrap();
        assert_eq!(settings.button_corner_radius, 8);
        settings.button_corner_radius = 24;
        let restored: CacheSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.button_corner_radius, 24);
    }

    #[test]
    fn accent_color_migrates_and_round_trips_presets() {
        let mut legacy = serde_json::to_value(CacheSettings::default()).unwrap();
        legacy.as_object_mut().unwrap().remove("accent_color");
        let settings: CacheSettings = serde_json::from_value(legacy).unwrap();
        assert_eq!(settings.accent_color, AccentColor::Purple);
        assert_eq!(AccentColor::from_index(-1), AccentColor::Purple);
        assert_eq!(AccentColor::from_index(8), AccentColor::Purple);
        for index in 0..8 {
            let mut settings = settings.clone();
            settings.accent_color = AccentColor::from_index(index);
            assert_eq!(settings.accent_color.index(), index);
            let encoded = serde_json::to_string(&settings).unwrap();
            let restored: CacheSettings = serde_json::from_str(&encoded).unwrap();
            assert_eq!(restored.accent_color, settings.accent_color);
        }
    }

    #[test]
    fn background_style_migrates_legacy_black_and_preserves_explicit_choices() {
        let legacy = serde_json::json!({"enabled":false,"format":"webp","quality":85,"true_black":true,
            "sync_overrides":{"true_black":false}});
        let mut settings: CacheSettings = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(settings.background_style, BackgroundStyle::TrueBlack);
        assert_eq!(
            settings.sync_overrides["background_style"],
            serde_json::json!("default")
        );
        assert!(!settings.sync_overrides.contains_key("true_black"));
        for index in 0..3 {
            settings.background_style = BackgroundStyle::from_index(index);
            let encoded = serde_json::to_value(&settings).unwrap();
            assert!(encoded.get("true_black").is_none());
            let restored: CacheSettings = serde_json::from_value(encoded).unwrap();
            assert_eq!(restored.background_style.index(), index);
        }
        let mut explicit = legacy;
        explicit["background_style"] = serde_json::json!("accent_tinted");
        explicit["sync_overrides"]["background_style"] = serde_json::json!("true_black");
        let explicit: CacheSettings = serde_json::from_value(explicit).unwrap();
        assert_eq!(explicit.background_style, BackgroundStyle::AccentTinted);
        assert_eq!(
            explicit.sync_overrides["background_style"],
            serde_json::json!("true_black")
        );
        assert_eq!(
            CacheSettings::default().background_style,
            BackgroundStyle::Default
        );
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

    #[test]
    fn remote_control_preference_defaults_on_and_round_trips() {
        let mut settings: CacheSettings =
            serde_json::from_str(r#"{"enabled":false,"format":"webp","quality":85}"#).unwrap();
        assert!(settings.remote_control_enabled);
        for enabled in [false, true] {
            settings.remote_control_enabled = enabled;
            let saved = serde_json::to_string(&settings).unwrap();
            let restored: CacheSettings = serde_json::from_str(&saved).unwrap();
            assert_eq!(restored.remote_control_enabled, enabled);
        }
    }

    #[test]
    fn completion_prompts_migrate_enabled_and_preserve_explicit_opt_out() {
        let mut settings: CacheSettings =
            serde_json::from_str(r#"{"enabled":false,"format":"webp","quality":85}"#).unwrap();
        assert!(settings.completion_rating_prompts_enabled);
        settings.completion_rating_prompts_enabled = false;
        let restored: CacheSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(!restored.completion_rating_prompts_enabled);
    }

    #[test]
    fn home_devices_defaults_visible_and_preserves_saved_preference() {
        assert!(CacheSettings::default().home_devices_enabled);
        let older = r#"{"enabled":false,"format":"webp","quality":85,"downscale":true}"#;
        let mut settings: CacheSettings = serde_json::from_str(older).unwrap();
        assert!(settings.home_devices_enabled);
        for visible in [false, true] {
            settings.home_devices_enabled = visible;
            let restored: CacheSettings =
                serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert_eq!(restored.home_devices_enabled, visible);
        }
    }

    #[test]
    fn home_artwork_defaults_to_posters_and_preserves_saved_episode_preference() {
        assert!(!CacheSettings::default().home_episode_artwork);
        let older = r#"{"enabled":false,"format":"webp","quality":85,"downscale":true}"#;
        let mut settings: CacheSettings = serde_json::from_str(older).unwrap();
        assert!(!settings.home_episode_artwork);
        for episode_artwork in [true, false] {
            settings.home_episode_artwork = episode_artwork;
            let restored: CacheSettings =
                serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert_eq!(restored.home_episode_artwork, episode_artwork);
        }
    }

    #[test]
    fn screenshot_defaults_migrate_and_preserve_format_quality_and_folders() {
        let older = r#"{"enabled":false,"format":"webp","quality":85,"downscale":true}"#;
        let mut settings: CacheSettings = serde_json::from_str(older).unwrap();
        assert_eq!(settings.screenshot_format, ScreenshotFormat::Png);
        assert_eq!(settings.screenshot_jpeg_quality, 85);
        assert_eq!(settings.screenshot_webp_quality, 85);
        assert!(!settings.screenshot_show_folder);
        assert_eq!(
            settings.screenshot_format,
            CacheSettings::default().screenshot_format
        );
        for index in 0..4 {
            settings.screenshot_format = ScreenshotFormat::from_index(index);
            settings.screenshot_jpeg_quality = 67;
            settings.screenshot_webp_quality = 92;
            settings.screenshot_show_folder = true;
            let restored: CacheSettings =
                serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert_eq!(restored, settings);
            assert_eq!(restored.screenshot_format.index(), index);
        }
    }
}

#[cfg(test)]
mod image_resolution_tests {
    use super::*;

    fn settings(extra: serde_json::Value) -> serde_json::Value {
        let mut value = serde_json::json!({"enabled":false,"format":"webp","quality":85});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        value
    }

    #[test]
    fn missing_fields_migrate_but_explicit_original_wins() {
        let defaults: CacheSettings =
            serde_json::from_value(settings(serde_json::json!({}))).unwrap();
        assert_eq!(defaults.detail_poster_max_side, Some(480));
        assert_eq!(defaults.detail_backdrop_max_side, Some(960));
        assert_eq!(defaults.episode_thumbnail_max_side, Some(360));
        assert_eq!(defaults.player_episode_backdrop_max_side, None);
        assert_eq!(defaults.image_disk_cache_limit_mb, Some(1024));
        let unlimited: CacheSettings = serde_json::from_value(settings(
            serde_json::json!({"image_disk_cache_limit_mb":null}),
        ))
        .unwrap();
        assert_eq!(unlimited.image_disk_cache_limit_mb, None);
        assert_eq!(
            serde_json::to_value(&unlimited).unwrap()["image_disk_cache_limit_mb"],
            serde_json::Value::Null
        );
        let legacy: CacheSettings = serde_json::from_value(settings(
            serde_json::json!({"downscale":true,"lazy_reencode":true,"rewrite_existing":true}),
        ))
        .unwrap();
        assert_eq!(legacy.player_episode_backdrop_max_side, Some(1024));
        assert_eq!(legacy.detail_poster_max_side, Some(480));
        let explicit: CacheSettings = serde_json::from_value(settings(serde_json::json!({"downscale":true,"player_episode_backdrop_max_side":null,"detail_poster_max_side":1920}))).unwrap();
        assert_eq!(explicit.player_episode_backdrop_max_side, None);
        assert_eq!(explicit.detail_poster_max_side, Some(1920));
        let serialized = serde_json::to_value(&legacy).unwrap();
        for key in ["downscale", "lazy_reencode", "rewrite_existing"] {
            assert!(serialized.get(key).is_none());
        }
    }

    #[test]
    fn all_resolution_fields_validate_and_round_trip() {
        for field in [
            "detail_poster_max_side",
            "detail_backdrop_max_side",
            "episode_thumbnail_max_side",
            "player_episode_backdrop_max_side",
        ] {
            for preset in IMAGE_RESOLUTION_PRESETS
                .iter()
                .copied()
                .map(Some)
                .chain([None])
            {
                let mut value = settings(serde_json::json!({}));
                value[field] = serde_json::to_value(preset).unwrap();
                let parsed: CacheSettings = serde_json::from_value(value).unwrap();
                assert_eq!(
                    serde_json::to_value(&parsed).unwrap()[field],
                    serde_json::to_value(preset).unwrap()
                );
            }
            for invalid in [
                serde_json::json!(0),
                serde_json::json!(999),
                serde_json::json!(-240),
                serde_json::json!(480.5),
                serde_json::json!("480"),
            ] {
                let mut value = settings(serde_json::json!({}));
                value[field] = invalid;
                assert!(serde_json::from_value::<CacheSettings>(value).is_err());
            }
        }
    }
}

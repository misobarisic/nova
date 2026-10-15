// The generated Slint components: the catalog AppWindow, whose last child is
// the player overlay used by both the mpv and the HTML5-video players.
//
// Kept in its own leaf crate so edits to the app logic (crate `nova`'s
// `src/app.rs`) no longer re-expand and re-typecheck this large generated
// code on every debug rebuild — it only rebuilds when a `.slint` file changes.
pub mod backend_text;
mod fonts;
pub use fonts::initialize_fonts;

mod generated {
    slint::include_modules!();
}
// Model types and globals preserve their existing public paths. AppWindow's
// wrapper installs the factory for production and headless callers alike.
pub use generated::*;
// Imported library models are not re-exported by Slint 1.18's generator.
// Preserve Nova's public model paths while sharing the leaf's exact types.
pub use nova_ui_shared::{
    AddonRow, CalCell, CalendarDay, CalendarRelease, CategoryRow, ContinueRow, DownloadEpisode,
    DownloadGroup, DownloadRow, EpisodeRow, HomeCatalogCard, HomeCatalogRow, HomeCatalogSection,
    LicenseSource, MediaCard, PluginSettingRow, RemoteDeviceRow, RemoteOfferRow, SeasonCard,
    SettingSyncInfo, SheetItem, StreamRow, SyncInvite, SyncPeer, ThemePalette, TrackRow,
    TrackingAccountRow, TrackingCandidateRow, TrackingLinkRow, TrackingMappingRow,
    TrackingSetupRow, UpcomingRow,
};
// Native page callbacks route through AppWindow without keeping it alive.
macro_rules! forward_app_callbacks {
    ($app:expr, $bridge:expr; $($callback:ident => $invoke:ident($($arg:ident),*)),* $(,)?) => {{
        let bridge = $bridge;
        $(let weak = $app.as_weak();
        bridge.$callback(move |$($arg),*| {
            weak.upgrade().map(|app| app.$invoke($($arg),*)).unwrap_or_default()
        });)*
    }};
}
pub(crate) use forward_app_callbacks;

mod collection_factories;
mod page_factories;
mod playback_factories;
mod remote_bridge;
mod settings_factory;
mod window;
pub use window::AppWindow;

/// Local AND search over Unicode lowercase, whitespace-separated terms.
/// The caller supplies localized labels and the registry's English aliases.
pub fn settings_search_matches(query: &str, haystack: &str) -> bool {
    let haystack = haystack.to_lowercase();
    query
        .split_whitespace()
        .all(|term| haystack.contains(&term.to_lowercase()))
}

/// Apply a device's display preference without remounting any controls.
/// Always derive from the standard palette so toggling off restores every token.
pub fn apply_theme(app: &AppWindow, true_black: bool) {
    use slint::ComponentHandle;
    let theme = app.global::<Theme>();
    let mut palette = theme.get_standard();
    if true_black {
        let black = slint::Color::from_rgb_u8(0, 0, 0);
        palette.canvas = black;
        palette.artwork_canvas = black;
        palette.artwork_backdrop = black;
        palette.nav_bottom = black;
        palette.nav_rail = black;
        palette.scrim_artwork = black;
        palette.episode_card = black;
        palette.scrim_episode_card = black;
        palette.season_scrim = theme.get_black_season_scrim();
        // True black also flattens neutral cards and controls. Keep accents,
        // status badges and artwork intact, and use outlines for separation.
        palette.card = black.into();
        palette.popup = black;
        palette.category_panel = black;
        palette.control = black;
        palette.hero_control = black;
        palette.hero_metadata_badge = black;
        palette.settings_control = black;
        palette.stream_card = black;
        palette.season_card = black;
        palette.episode_placeholder = black;
        palette.episode_toolbar = black;
        palette.episode_search = black;
        palette.episode_empty_panel = black;
        palette.home_artwork_placeholder = black;
        palette.calendar_control = black;
        palette.tracking_control = black;
        palette.tracking_mapping = black;
        palette.tracking_panel = black;
        palette.tracking_service_control = black;
        palette.player_menu = black;
        let hover = slint::Color::from_rgb_u8(18, 18, 18);
        palette.control_hover = hover;
        palette.hero_control_hover = hover;
        palette.detail_filter_hover = hover;
        palette.input_hover = hover;
        palette.settings_control_hover = hover;
        palette.tracking_control_hover = hover;
        palette.popup_divider = slint::Color::from_rgb_u8(32, 32, 32);
        let outline = slint::Color::from_rgb_u8(48, 48, 48);
        palette.control_border = outline;
        palette.step_border = outline;
        palette.border_soft = slint::Color::from_argb_u8(48, 255, 255, 255);
        palette.border_card = slint::Color::from_argb_u8(32, 255, 255, 255);
        palette.border_subtle = slint::Color::from_argb_u8(24, 255, 255, 255);
    }
    theme.set_button_corner_radius(app.get_button_corner_radius().round().clamp(0.0, 24.0));
    theme.set_card_corner_radius(app.get_card_corner_radius().round().clamp(0.0, 24.0));
    theme.set_card_spacing(app.get_card_spacing().round().clamp(0.0, 32.0));
    theme.set_status_bar_gradient(app.get_status_bar_gradient().round().clamp(0.0, 100.0));
    theme.set_home_backdrop_size(app.get_home_backdrop_size().clamp(0, 2));
    theme.set_detail_backdrop_size(app.get_detail_backdrop_size().clamp(0, 2));
    theme.set_hero_title_alignment(app.get_hero_title_alignment().clamp(0, 2));
    theme.set_true_black(true_black);
    theme.set_current(palette);
}

#[cfg(test)]
mod search_tests {
    #[test]
    fn matches_all_terms_in_any_order_and_language() {
        use super::settings_search_matches as matches;
        assert!(matches("  WEBP  quality ", "Kvaliteta JPEG WebP Quality"));
        assert!(matches("kvaliteta", "Kvaliteta JPEG WebP Quality"));
        assert!(matches("", "Home"));
        assert!(!matches("quality tracking", "Quality JPEG WebP"));
    }
}

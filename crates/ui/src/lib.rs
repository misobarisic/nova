// The generated Slint components: the catalog AppWindow, whose last child is
// the player overlay used by both the mpv and the HTML5-video players.
//
// Kept in its own leaf crate so edits to the app logic (crate `nova`'s
// `src/app.rs`) no longer re-expand and re-typecheck this large generated
// code on every debug rebuild — it only rebuilds when a `.slint` file changes.
pub mod backend_text;

slint::include_modules!();

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
    }
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

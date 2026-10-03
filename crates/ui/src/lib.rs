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

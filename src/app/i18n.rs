//! UI language (Settings → Display → Language).
//!
//! The interface strings are Slint's: user-facing literals in
//! `crates/ui/*.slint` are wrapped in `@tr("…")` (the Settings page is
//! translated; the other pages follow), and `crates/ui/build.rs` bundles the
//! gettext catalogs under `crates/ui/translations/<code>/` into the binary.
//! Switching language is therefore a single Slint call — it selects the catalog
//! whose folder name is the language code and marks every translation dirty, so
//! all `@tr` bindings re-evaluate in place. No page is rebuilt and no string is
//! pushed from here.
//!
//! English is the source language, so `"en"` restores the built-in strings and
//! is the fallback whenever a catalog (or a single string) is missing; Croatian
//! (`hr`) is the first real catalog (Settings page + section headers).
//!
//! Applying the language is *not* part of the settings blob being loaded: the
//! catalogs are looked up in the UI crate, so the app mirrors the choice onto
//! the UI the same way `apply_animations` does (startup + after a save) rather
//! than in `nova-config`, which must stay UI-free.
use super::*;
use std::cell::RefCell;

/// Language names for the Settings → Display picker, in `Language::ALL` order.
/// Each is written in the language itself (`label()`), so they are shown as-is
/// in every language.
pub(crate) fn language_labels() -> Vec<SharedString> {
    Language::ALL
        .iter()
        .map(|language| SharedString::from(language.label()))
        .collect()
}

/// State of the bundled catalogs, so `apply_language` can be called from every
/// settings mirror (page open, autosave, sync apply, startup) without
/// re-selecting the same language or re-reporting a build that has none.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
enum Catalogs {
    /// Nothing selected yet.
    #[default]
    Unset,
    /// A language is in place (Slint's selected catalog).
    Selected(&'static str),
    /// `select_bundled_translation` failed: this build has no catalogs at all,
    /// so stop trying — and stop logging — on this thread.
    ///
    /// That is the interpreter-backed `live-preview` UI (`cargo dev`): it
    /// compiles the `.slint` files at runtime and never registers a bundle
    /// (`set_bundled_languages` is only emitted by the ahead-of-time
    /// generator), so the strings stay English there.
    Unavailable,
}

impl Catalogs {
    /// Whether `code` still has to be pushed into Slint.
    fn needs_select(self, code: &str) -> bool {
        match self {
            Catalogs::Unset => true,
            Catalogs::Selected(selected) => selected != code,
            Catalogs::Unavailable => false,
        }
    }
}

thread_local! {
    /// See [`Catalogs`]. Thread-local, like Slint's own context: the selected
    /// catalog lives on the thread that created the window, so a window built
    /// on a fresh UI thread (an Android activity restart) starts from `Unset`
    /// and re-selects the stored language instead of skipping it as "already
    /// applied".
    static CATALOGS: RefCell<Catalogs> = const { RefCell::new(Catalogs::Unset) };
}

impl Bridge {
    /// Point Slint's catalogs at the stored language. Called on startup
    /// (through `settings_to_ui`) and after every settings save, so a pick in
    /// Settings → Display applies as soon as the page's autosave debounce
    /// lands — same cadence as the animation switches.
    pub(super) fn apply_language(&self, settings: &CacheSettings) {
        let code = settings.language.code();
        // Cheap no-op paths: the language is already in place, or this build
        // has no catalogs (never re-log those). Both leave the backend text
        // (`text.rs`) as the selection below set it.
        if !CATALOGS.with(|state| state.borrow().needs_select(code)) {
            return;
        }
        // No component yet (Android headless JNI paths) — nothing to
        // retranslate, but keep the backend text in step with what a window
        // would show.
        if self.app().is_none() {
            text::set_language(settings.language);
            return;
        }
        let mut failure = None;
        CATALOGS.with(|state| match slint::select_bundled_translation(code) {
            Ok(()) => {
                *state.borrow_mut() = Catalogs::Selected(code);
                // The backend-built text follows whatever the UI can show.
                text::set_language(settings.language);
            }
            Err(err) => {
                *state.borrow_mut() = Catalogs::Unavailable;
                text::set_language(Language::English);
                failure = Some(err);
            }
        });
        if let Some(err) = failure {
            eprintln!(
                "nova: UI translations are not bundled in this build ({err}) — \
                 the interface stays in English. The interpreter-backed UI \
                 (`cargo dev` / `live-preview`) has no catalogs; a regular \
                 `cargo build` / `cargo run` bundles them."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `apply_language` runs from every settings mirror, so the selection is
    /// skipped unless it actually changes — and a build without catalogs stops
    /// asking (and logging) after the first failure.
    #[test]
    fn catalog_selection_is_skipped_when_nothing_changes() {
        assert!(Catalogs::Unset.needs_select("en"));
        assert!(Catalogs::Unset.needs_select("hr"));

        assert!(!Catalogs::Selected("en").needs_select("en"));
        assert!(Catalogs::Selected("en").needs_select("hr"));
        assert!(Catalogs::Selected("hr").needs_select("en"));

        assert!(!Catalogs::Unavailable.needs_select("en"));
        assert!(!Catalogs::Unavailable.needs_select("hr"));
    }
}

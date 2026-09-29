fn main() {
    // Debug info feeds the `ElementHandle` queries used by the headless
    // drag-scroll regression test (`tests/scroll_drag.rs`, driven via
    // i-slint-backend-testing). It only bloats debug builds; release
    // builds stay lean unless SLINT_EMIT_DEBUG_INFO is set explicitly.
    let debug_info = std::env::var("SLINT_EMIT_DEBUG_INFO")
        .map(|v| v != "0" && v.to_lowercase() != "false")
        .unwrap_or_else(|_| std::env::var("PROFILE").as_deref() == Ok("debug"));

    // In Linux debug builds with the opt-in `live-preview` feature, Slint emits
    // an interpreter-backed component that loads `appwindow.slint` from disk and
    // hot-reloads it, instead of ahead-of-time compiling it. Every other case
    // (other targets, release, feature off) stays AOT. Setting SLINT_LIVE_PREVIEW
    // explicitly overrides the profile-based default.
    if std::env::var_os("SLINT_LIVE_PREVIEW").is_none() {
        let linux = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux");
        let debug = std::env::var("PROFILE").as_deref() == Ok("debug");
        let live_preview = std::env::var_os("CARGO_FEATURE_LIVE_PREVIEW").is_some();
        if linux && debug && live_preview {
            // SAFETY: build scripts run single-threaded, before slint-build
            // (which reads this in-process) is invoked.
            unsafe { std::env::set_var("SLINT_LIVE_PREVIEW", "1") };
        }
    }

    // UI translations: the `@tr("…")` strings in the `.slint` files are looked
    // up in the gettext catalogs bundled from `translations/` at compile time,
    // keyed by the language selected in Settings → Display
    // (`slint::select_bundled_translation`, see `src/app/i18n.rs`). The domain
    // is this crate's package name (`nova-ui`), so a catalog is
    // `translations/<code>/LC_MESSAGES/nova-ui.po`.
    //
    // Catalogs are keyed by the source string alone. The default translation
    // context is the *component* a string sits in, so moving a `@tr` literal to
    // another component (or reusing one) would silently stop matching its
    // entry — hence `DefaultTranslationContext::None`, which the extractor
    // mirrors with `slint-tr-extractor --no-default-translation-context`. A
    // string that genuinely means two things in two places can still name its
    // context (`@tr("ctx" => "…")`).
    slint_build::compile_with_config(
        "appwindow.slint",
        slint_build::CompilerConfiguration::new()
            .with_debug_info(debug_info)
            .with_bundled_translations("translations")
            .with_default_translation_context(slint_build::DefaultTranslationContext::None),
    )
    .expect("failed to compile ui/appwindow.slint");
}

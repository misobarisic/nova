include!("build-support.rs");

fn main() {
    enable_experimental_components();
    println!("cargo:rerun-if-changed=build-support.rs");
    // Debug info feeds the `ElementHandle` queries used by the headless
    // drag-scroll regression test (`tests/scroll_drag.rs`, driven via
    // i-slint-backend-testing). It only bloats debug builds; release
    // builds stay lean unless SLINT_EMIT_DEBUG_INFO is set explicitly.
    let debug_info = inspection_metadata();

    // The page factory bridges shares native compiled-library globals.
    // Interpreter globals/weak handles have a different representation, so
    // keep all compilation units AOT until there is a separate preview bridge.
    // SAFETY: this build script has not started any threads.
    unsafe { std::env::remove_var("SLINT_LIVE_PREVIEW") };
    if std::env::var_os("CARGO_FEATURE_LIVE_PREVIEW").is_some() {
        println!(
            "cargo:warning=live-preview currently uses compiled UI with compiled page modules"
        );
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
            // Imported fonts must ship in the executable/APK, rather than
            // depend on absolute paths into the build machine's checkout.
            .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles)
            .with_debug_info(debug_info)
            .with_bundled_translations("translations")
            .with_default_translation_context(slint_build::DefaultTranslationContext::None),
    )
    .expect("failed to compile ui/appwindow.slint");
}

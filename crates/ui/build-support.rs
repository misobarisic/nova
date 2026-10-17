fn enable_experimental_components() {
    // SAFETY: UI build scripts do not start threads before configuring Slint.
    unsafe {
        std::env::set_var("SLINT_ENABLE_EXPERIMENTAL_FEATURES", "1");
        std::env::set_var("SLINT_BUNDLE_TRANSLATIONS_DOMAIN", "nova-ui");
    };
}

fn inspection_metadata() -> bool {
    std::env::var("SLINT_EMIT_DEBUG_INFO")
        .map(|value| value != "0" && value.to_lowercase() != "false")
        .unwrap_or_else(|_| std::env::var("PROFILE").as_deref() == Ok("debug"))
}

fn compiler_configuration() -> slint_build::CompilerConfiguration {
    let config = slint_build::CompilerConfiguration::new();
    // Windows uses Slint-rendered menus instead of the native Win32 popup.
    // Compile every page with the same dark widgets so those menus cannot
    // inherit a light Windows system theme inside Nova's dark interface.
    // Use the target OS: Windows releases are also cross-compiled on Linux.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        config.with_style("fluent-dark".into())
    } else {
        config
    }
}

// Every compiled unit must bundle the same languages in the same order. The
// compiler patch lets libraries read the canonical domain without changing
// Cargo's package/library identity or generating catalogs that watch themselves.
#[allow(dead_code)] // AppWindow uses its source-domain catalogs directly.
fn compile_page_library(source: &str, library: &str) {
    enable_experimental_components();
    // SAFETY: build scripts configure Slint before starting any threads.
    unsafe { std::env::remove_var("SLINT_LIVE_PREVIEW") };
    println!("cargo:rerun-if-changed=../ui/build-support.rs");
    println!("cargo:rerun-if-changed=../ui/translations");
    slint_build::compile_with_config(
        source,
        compiler_configuration()
            .as_library(library)
            .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles)
            .with_debug_info(inspection_metadata())
            .with_bundled_translations("../ui/translations")
            .with_default_translation_context(slint_build::DefaultTranslationContext::None),
    )
    .expect("compile UI library");
}

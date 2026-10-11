fn compile(library: Option<&str>) {
    let debug_info = std::env::var("SLINT_EMIT_DEBUG_INFO")
        .map(|value| value != "0" && value.to_lowercase() != "false")
        .unwrap_or_else(|_| std::env::var("PROFILE").as_deref() == Ok("debug"));
    let mut config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles)
        .with_debug_info(debug_info)
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    if let Some(name) = library {
        config = config.as_library(name);
    }
    println!("cargo:rerun-if-changed=../build-support.rs");
    slint_build::compile_with_config("ui.slint", config).expect("compile Slint module experiment");
}

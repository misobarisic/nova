include!("../ui/build-support.rs");

fn main() {
    enable_experimental_components();
    // SAFETY: the build script has not started threads.
    unsafe { std::env::remove_var("SLINT_LIVE_PREVIEW") };
    println!("cargo:rerun-if-changed=../ui/build-support.rs");
    println!("cargo:rerun-if-changed=../ui/translations");
    slint_build::compile_with_config(
        "sync-module.slint",
        compiler_configuration()
            .as_library("NovaSettingsSync")
            .with_library_paths(std::collections::HashMap::from([(
                "NovaSyncHost".into(),
                std::path::PathBuf::from("sync-host.slint"),
            )]))
            .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles)
            .with_debug_info(inspection_metadata())
            .with_bundled_translations("../ui/translations")
            .with_default_translation_context(slint_build::DefaultTranslationContext::None),
    )
    .expect("compile shared sync UI library");
}

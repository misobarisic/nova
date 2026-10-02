use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    generate_license_catalog();
    // The Slint UI is compiled by the `nova-ui` crate's build script. This
    // root script generates the About catalog and emits native link directives.

    // The mpv render API / libmpv2 (via the `nova-player` crate) needs to link
    // against X11 on native Linux.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "linux" {
        println!("cargo:rustc-link-lib=X11");
    }

    // Windows: link to the pinned MinGW import library for libmpv. The matching
    // DLL is staged next to nova.exe by the tag-release packaging script.
    if target_os == "windows" {
        println!("cargo:rerun-if-env-changed=MPV_SOURCE");
        let source = std::env::var_os("MPV_SOURCE")
            .map(std::path::PathBuf::from)
            .expect(
                "MPV_SOURCE must point to the Windows libmpv package (use nix develop .#windows)",
            );
        let lib_dir = source.join("64");
        let import_library = lib_dir.join("libmpv.dll.a");
        if !import_library.is_file() {
            panic!(
                "Windows libmpv import library not found at {}; see vendor/windows-libs/SOURCES",
                import_library.display()
            );
        }
        println!("cargo:rustc-link-search=native={}", lib_dir.display());
    }

    // Android: `libmpv2-sys` emits a bare `cargo:rustc-link-lib=mpv`, so the
    // per-ABI search path has to come from here. It points at the prebuilt
    // libmpv.so vendored in vendor/android-libs/<android_abi>/ — the same files
    // cargo-apk packs into lib/<abi>/ of the APK (Cargo.toml `runtime_libs`;
    // provenance and hashes in vendor/android-libs/SOURCES). The ABI directory names
    // are Gradle's, not the Rust target triples, hence the mapping.
    if target_os == "android" {
        println!("cargo:rerun-if-changed=vendor/android-libs");
        let target = std::env::var("TARGET").unwrap_or_default();
        match android_lib_dir(&target) {
            Some(dir) => println!("cargo:rustc-link-search=native=vendor/android-libs/{dir}"),
            None => println!(
                "cargo:warning=nova: no vendored libmpv.so for {target} — linking will fail; \
                 see vendor/android-libs/SOURCES for the vendored ABIs"
            ),
        }
    }
}

/// Generate the license and source catalog compiled into Settings → About.
fn generate_license_catalog() {
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "about.toml",
        "assets/open_source_licenses.hbs",
        "assets/open_source_crate_sources.hbs",
        "assets/open_source_project_sources.tsv",
        "assets/open_source_vendors.txt",
        "LICENSE",
        "assets/fonts/LICENSE.txt",
        "crates/addons/Cargo.toml",
        "crates/ui/Cargo.toml",
        "crates/storage/Cargo.toml",
        "crates/config/Cargo.toml",
        "crates/download/Cargo.toml",
        "crates/torrent/Cargo.toml",
        "crates/media/Cargo.toml",
        "crates/providers/Cargo.toml",
        "crates/providers/plugins/anikoto/NOTICE.md",
        "crates/player/Cargo.toml",
        "crates/sync/Cargo.toml",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    for name in [
        "TARGET",
        "CARGO_FEATURE_DESKTOP",
        "CARGO_FEATURE_LIVE_PREVIEW",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }

    let target = env::var("TARGET").unwrap_or_else(|_| "unknown-target".into());
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    let mut sources = render_cargo_about(
        &manifest_dir,
        "assets/open_source_crate_sources.hbs",
        &target,
    );
    sources.push_str(include_str!("assets/open_source_project_sources.tsv"));

    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".into());
    let mut catalog =
        format!("NOVA — OPEN-SOURCE LICENSES\nTarget: {target}\nVersion: {version}\n\n",);
    catalog.push_str(&render_cargo_about(
        &manifest_dir,
        "assets/open_source_licenses.hbs",
        &target,
    ));
    catalog.push_str("\n\n");
    catalog.push_str(include_str!("assets/open_source_vendors.txt"));
    catalog.push_str("\n\n===== Nova project license: GPL-3.0-or-later =====\n\n");
    catalog.push_str(include_str!("LICENSE"));
    catalog.push_str("\n\n===== Roboto font license: Apache-2.0 =====\n\n");
    catalog.push_str(include_str!("assets/fonts/LICENSE.txt"));
    catalog.push_str("\n\n===== AniKoto JavaScript provider =====\n\n");
    catalog.push_str(include_str!("crates/providers/plugins/anikoto/NOTICE.md"));
    // The complete Apache-2.0 terms are included by the font notice above.

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    fs::write(out_dir.join("nova_license_catalog.txt"), catalog)
        .expect("write generated license catalog");
    fs::write(out_dir.join("nova_license_sources.tsv"), sources)
        .expect("write generated license source links");
}

fn render_cargo_about(manifest_dir: &PathBuf, template: &str, target: &str) -> String {
    let mut command = Command::new("cargo-about");
    command
        .current_dir(manifest_dir)
        .arg("generate")
        .arg("--locked")
        .arg("--fail")
        .arg("-c")
        .arg("about.toml")
        .arg("--target")
        .arg(target);

    let mut features = Vec::new();
    if env::var_os("CARGO_FEATURE_DESKTOP").is_some() {
        features.push("desktop");
    }
    if env::var_os("CARGO_FEATURE_LIVE_PREVIEW").is_some() {
        features.push("live-preview");
    }
    if features.is_empty() {
        command.arg("--no-default-features");
    } else {
        command.arg("--features").arg(features.join(","));
    }

    let output = command
        .arg(template)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "license catalog generation needs cargo-about; run nix develop (or nix develop .#android): {error}"
            )
        });
    if !output.status.success() {
        panic!(
            "cargo-about failed to generate {template}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).unwrap_or_else(|error| {
        panic!("cargo-about returned non-UTF-8 output for {template}: {error}")
    })
}

/// Map a Rust target triple to its Gradle/`libmpv-android-video-build` ABI
/// directory under `vendor/android-libs/`, if one is vendored there.
fn android_lib_dir(target: &str) -> Option<&'static str> {
    let abi = match target {
        "aarch64-linux-android" => "arm64-v8a",
        "x86_64-linux-android" => "x86_64",
        "armv7-linux-androideabi" | "thumbv7neon-linux-androideabi" => "armeabi-v7a",
        "i686-linux-android" => "x86",
        _ => return None,
    };
    // Only advertise a directory that actually holds the library: a stale or
    // missing file should surface as a warning here rather than as an
    // obscure "cannot find -lmpv" from the linker.
    let lib = std::path::Path::new("vendor/android-libs")
        .join(abi)
        .join("libmpv.so");
    lib.exists().then_some(abi)
}

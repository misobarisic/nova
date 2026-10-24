use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "build-support/version.rs"]
mod version;

fn main() {
    generate_build_version();
    generate_license_catalog();
    // Cargo exposes library metadata to build scripts. Forward it to rustc so
    // inline slint! test fixtures resolve the canonical compiled shared globals.
    for field in ["NAME", "SOURCE", "PACKAGE", "MODULE"] {
        let key = format!("DEP_NOVAUISHARED_SLINT_LIBRARY_{field}");
        println!("cargo:rerun-if-env-changed={key}");
        if let Ok(value) = env::var(&key) {
            println!("cargo:rustc-env={key}={value}");
        }
    }
    // Shared bridge declarations contain experimental ComponentFactory types.
    println!("cargo:rustc-env=SLINT_ENABLE_EXPERIMENTAL_FEATURES=1");
    // The Slint UI is compiled by the `nova-ui` crate's build script. This
    // root script generates the About catalog and emits native link directives.

    // The mpv render API / libmpv2 (via the `nova-player` crate) needs to link
    // against X11 on native Linux.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "linux" {
        println!("cargo:rustc-link-lib=X11");
    }

    // Windows: link to the source-built MinGW import library for libmpv. The matching
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
                "Windows libmpv import library not found at {}; see nix/native-media/README.md",
                import_library.display()
            );
        }
        println!("cargo:rustc-link-search=native={}", lib_dir.display());
    }

    // The shell and cargo-apk2 use the same immutable Nix build output. FFmpeg
    // is shared separately, including avcodec's JavaVM registration API.
    if target_os == "android" {
        println!("cargo:rerun-if-env-changed=NOVA_ANDROID_LIBS");
        let source = env::var_os("NOVA_ANDROID_LIBS").map(PathBuf::from).expect(
            "NOVA_ANDROID_LIBS must point to the built Android media directory (use nix develop .#android)",
        );
        let target = env::var("TARGET").unwrap_or_default();
        let abi =
            android_abi(&target).unwrap_or_else(|| panic!("unsupported Android target: {target}"));
        let directory = source.join(abi);
        for name in [
            "mpv",
            "avcodec",
            "avformat",
            "avfilter",
            "avutil",
            "swscale",
            "swresample",
        ] {
            let library = directory.join(format!("lib{name}.so"));
            assert!(
                library.is_file(),
                "missing native library {}",
                library.display()
            );
        }
        println!("cargo:rustc-link-search=native={}", directory.display());
    }
}

fn generate_build_version() {
    println!("cargo:rerun-if-changed=build-support/version.rs");
    // A tag/commit or a tracked source edit changes the identity even when the
    // Rust package version stays at the last release between tags.
    for path in version::SOURCE_PATHS {
        println!("cargo:rerun-if-changed={path}");
    }
    for path in ["HEAD", "refs", "packed-refs", "index", "logs/HEAD"] {
        if let Some(path) = version::git(&["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    for key in ["GITHUB_REF_TYPE", "GITHUB_REF_NAME"] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let package = env::var("CARGO_PKG_VERSION").expect("Cargo package version");
    if env::var("GITHUB_REF_TYPE").as_deref() == Ok("tag") {
        let tag = env::var("GITHUB_REF_NAME").expect("release tag");
        assert_eq!(
            tag,
            format!("v{package}"),
            "release tag must match Cargo.toml"
        );
    }
    let tag = version::git(&["describe", "--tags", "--match", "v[0-9]*", "--abbrev=0"]);
    let commit = version::git(&["rev-parse", "--short=8", "HEAD"]);
    let exact = (env::var("PROFILE").as_deref() == Ok("release"))
        .then(|| version::git(&["describe", "--tags", "--match", "v[0-9]*", "--exact-match"]))
        .flatten();
    let mut status_args = vec!["status", "--porcelain", "--untracked-files=normal", "--"];
    status_args.extend_from_slice(version::SOURCE_PATHS);
    let dirty = version::git(&status_args).is_none_or(|status| !status.is_empty());
    let info = version::resolve(
        &package,
        tag.as_deref(),
        commit.as_deref(),
        exact.as_deref(),
        dirty,
    );
    println!("cargo:rustc-env=NOVA_BUILD_VERSION={}", info.display);
    println!("cargo:rustc-env=NOVA_BASE_VERSION={}", info.base);
    println!("cargo:rustc-env=NOVA_BUILD_RELEASE={}", info.release);
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
        "assets/fonts/NotoColorEmoji-LICENSE.txt",
        "assets/icons/player/LICENSE",
        "crates/addons/Cargo.toml",
        "crates/ui/Cargo.toml",
        "crates/storage/Cargo.toml",
        "crates/config/Cargo.toml",
        "crates/download/Cargo.toml",
        "crates/torrent/Cargo.toml",
        "crates/media/Cargo.toml",
        "crates/providers/Cargo.toml",
        "crates/player/Cargo.toml",
        "crates/cast/Cargo.toml",
        "crates/cast-media/Cargo.toml",
        "crates/remux/Cargo.toml",
        "vendor/oxicast/Cargo.toml",
        "vendor/oxicast/NOVA_PATCH.md",
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
    if matches!(
        env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("android" | "windows")
    ) {
        println!("cargo:rerun-if-env-changed=NOVA_NATIVE_NOTICES");
        let path = env::var_os("NOVA_NATIVE_NOTICES").expect(
            "NOVA_NATIVE_NOTICES must point to the native notice file (use the target Nix shell)",
        );
        println!("cargo:rerun-if-changed={}", Path::new(&path).display());
        let notices = fs::read_to_string(&path).expect("could not read native dependency notices");
        catalog.push_str("\n\n");
        catalog.push_str(&notices);
    }
    catalog.push_str("\n\n===== Nova project license: GPL-3.0-or-later =====\n\n");
    catalog.push_str(include_str!("LICENSE"));
    catalog.push_str("\n\n===== Roboto font license: Apache-2.0 =====\n\n");
    catalog.push_str(include_str!("assets/fonts/LICENSE.txt"));
    catalog.push_str("\n\n===== Noto Color Emoji font license: OFL-1.1 =====\n\n");
    catalog.push_str(include_str!("assets/fonts/NotoColorEmoji-LICENSE.txt"));
    catalog.push_str("\n\n===== Lucide UI icons: ISC and Feather notices =====\n\n");
    catalog.push_str(include_str!("assets/icons/player/LICENSE"));

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let catalog_path = out_dir.join("nova_license_catalog.txt");
    let compressed_path = out_dir.join("nova_license_catalog.zst");
    let recipe_path = out_dir.join("nova_license_catalog.recipe");
    const COMPRESSION_LEVEL: i32 = 22;
    let recipe = format!("zstd:{COMPRESSION_LEVEL};checksum:true;size:true;v1\n");
    let cached = compressed_path.is_file()
        && fs::read(&catalog_path).is_ok_and(|old| old.as_slice() == catalog.as_bytes())
        && fs::read(&recipe_path).is_ok_and(|old| old.as_slice() == recipe.as_bytes());
    // The catalog rarely changes: spend a fraction of a second at build time
    // to minimize the embedded payload. Pledging its size also records it in
    // the frame, and the runtime uses the same exact size for its decode buffer.
    // Reuse the frame on unrelated build-script reruns. Comparing the original
    // bytes avoids cache hashes, and the recipe invalidates intentional codec
    // setting changes even when the legal text itself stays identical.
    if !cached {
        let mut encoder = zstd::stream::Encoder::new(Vec::new(), COMPRESSION_LEVEL)
            .expect("create license catalog compressor");
        encoder
            .include_checksum(true)
            .expect("checksum license catalog");
        encoder
            .set_pledged_src_size(Some(catalog.len() as u64))
            .expect("record license catalog size");
        encoder
            .write_all(catalog.as_bytes())
            .expect("compress license catalog");
        let compressed = encoder
            .finish()
            .expect("finish license catalog compression");
        write_if_changed(&compressed_path, &compressed).expect("write compressed license catalog");
        write_if_changed(&recipe_path, recipe.as_bytes())
            .expect("write license compression recipe");
    }
    write_if_changed(
        &out_dir.join("nova_license_catalog_size.rs"),
        format!(
            "pub(crate) const OPEN_SOURCE_LICENSE_CATALOG_SIZE: usize = {};\n",
            catalog.len()
        )
        .as_bytes(),
    )
    .expect("write license catalog size");
    // Retain the readable build artifact for inspection and exact round-trip
    // tests; the production binary embeds only the compressed file.
    write_if_changed(&catalog_path, catalog.as_bytes()).expect("write generated license catalog");
    write_if_changed(
        &out_dir.join("nova_license_sources.tsv"),
        sources.as_bytes(),
    )
    .expect("write generated license source links");
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if fs::read(path).is_ok_and(|old| old.as_slice() == bytes) {
        return Ok(());
    }
    fs::write(path, bytes)
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

/// Map Rust triples to the ABI directory names cargo-apk2 packages.
fn android_abi(target: &str) -> Option<&'static str> {
    match target {
        "aarch64-linux-android" => Some("arm64-v8a"),
        "x86_64-linux-android" => Some("x86_64"),
        "armv7-linux-androideabi" | "thumbv7neon-linux-androideabi" => Some("armeabi-v7a"),
        _ => None,
    }
}

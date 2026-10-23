use std::{env, path::PathBuf};
fn main() {
    println!("cargo:rerun-if-changed=src/packet_copy.c");
    println!("cargo:rerun-if-changed=src/stream_copy.c");
    for name in ["MPV_SOURCE", "NOVA_ANDROID_LIBS", "NOVA_FFMPEG_INCLUDE"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let target = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let mut build = cc::Build::new();
    build.file("src/packet_copy.c");
    if target == "windows" || target == "android" {
        let root = PathBuf::from(
            env::var(if target == "windows" {
                "MPV_SOURCE"
            } else {
                "NOVA_ANDROID_LIBS"
            })
            .expect("native media bundle must be configured"),
        );
        let abi = match env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
            "aarch64" => "arm64-v8a",
            "arm" => "armeabi-v7a",
            "x86_64" => "x86_64",
            _ => panic!("unsupported native media target"),
        };
        let default_include = if target == "android" {
            root.join("include").join(abi)
        } else {
            root.join("include")
        };
        let include = env::var_os("NOVA_FFMPEG_INCLUDE")
            .map(PathBuf::from)
            .unwrap_or_else(|| default_include);
        build.include(include);
        let lib = if target == "windows" {
            root.join("64")
        } else {
            root.join(abi)
        };
        println!("cargo:rustc-link-search=native={}", lib.display());
        for name in ["avformat", "avcodec", "avutil"] {
            println!("cargo:rustc-link-lib={name}");
        }
    } else {
        for name in ["libavformat", "libavcodec", "libavutil"] {
            let lib = pkg_config::Config::new()
                .probe(name)
                .expect("FFmpeg development headers and libraries are required");
            for include in lib.include_paths {
                build.include(include);
            }
        }
    }
    build.warnings(true).compile("nova_packet_copy");
}

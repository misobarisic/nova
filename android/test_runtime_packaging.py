#!/usr/bin/env python3
"""Exercise cargo-apk2's repeated staging of read-only Nix media libraries."""

import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import zipfile


def main():
    root = Path(__file__).resolve().parents[1]
    libraries = Path(os.environ["NOVA_ANDROID_LIBS"])
    abis = {
        "aarch64-linux-android": "arm64-v8a",
        "armv7-linux-androideabi": "armeabi-v7a",
        "x86_64-linux-android": "x86_64",
    }
    sources = sorted(libraries.glob("*/*.so"))
    assert len(sources) == 21, "expected seven media libraries per ABI"
    assert all(p.stat().st_mode & 0o222 == 0 for p in sources)
    hashes = {p: hashlib.sha256(p.read_bytes()).digest() for p in sources}

    with tempfile.TemporaryDirectory(prefix="nova-apk-runtime-") as scratch:
        work = Path(scratch)
        (work / "src").mkdir()
        (work / "Cargo.toml").write_text(
            f'''[package]
name = "runtime-packaging-probe"
version = "0.0.0"
edition = "2024"
[lib]
crate-type = ["cdylib"]
[package.metadata.android]
package = "dev.misob.nova.packagingprobe"
apk_name = "runtime-packaging-probe"
build_targets = ["aarch64-linux-android", "armv7-linux-androideabi", "x86_64-linux-android"]
use_aapt2 = true
[package.metadata.android.sdk]
min_sdk_version = 26
target_sdk_version = 35
[package.metadata.android.application]
has_code = false
[[package.metadata.android.application.activity]]
name = "android.app.NativeActivity"
exported = true
[package.metadata.android.signing.dev]
path = "{root / 'android/keystore/debug.keystore'}"
keystore_password = "android"
'''
        )
        # Linking avcodec makes recursive dependency staging overlap the explicit
        # runtime list. A rebuild also exercises copies left by the previous run.
        (work / "src/lib.rs").write_text(
            '''#[link(name = "avcodec")]
unsafe extern "C" { fn avcodec_version() -> u32; }
#[unsafe(no_mangle)]
pub extern "C" fn probe_version() -> u32 { unsafe { avcodec_version() } }
'''
        )
        (work / "build.rs").write_text(
            '''fn main() {
    let abi = match std::env::var("TARGET").unwrap().as_str() {
        "aarch64-linux-android" => "arm64-v8a",
        "armv7-linux-androideabi" => "armeabi-v7a",
        "x86_64-linux-android" => "x86_64",
        _ => panic!("unexpected target"),
    };
    println!("cargo:rustc-link-search=native={}/{}",
        std::env::var("NOVA_ANDROID_LIBS").unwrap(), abi);
}
'''
        )
        env = dict(os.environ, CARGO_TARGET_DIR=str(work / "target"), CARGO_NET_OFFLINE="true")
        for attempt in range(2):
            print(f"Packaging read-only media inputs, attempt {attempt + 1}", flush=True)
            subprocess.run(
                ["cargo", "apk2", "build", "--lib"],
                cwd=work,
                env=env,
                check=True,
            )
            apks = list((work / "target").rglob("runtime-packaging-probe.apk"))
            assert len(apks) == 1, apks
            with zipfile.ZipFile(apks[0]) as apk:
                entries = set(apk.namelist())
            for abi in abis.values():
                expected = {f"lib/{abi}/{p.name}" for p in sources if p.parent.name == abi}
                expected.add(f"lib/{abi}/libruntime_packaging_probe.so")
                assert expected <= entries, expected - entries

    assert all(hashlib.sha256(p.read_bytes()).digest() == digest for p, digest in hashes.items())
    assert all(p.stat().st_mode & 0o222 == 0 for p in sources)
    print("Both APK builds contain all three ABIs; Nix inputs remain unchanged.")


if __name__ == "__main__":
    main()

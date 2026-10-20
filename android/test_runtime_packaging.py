#!/usr/bin/env python3
"""Exercise cargo-apk2's repeated staging of read-only Nix media libraries."""

import hashlib
import importlib.util
import os
from pathlib import Path
import re
import subprocess
import tempfile
import zipfile


def verify_runtime_symbols(libraries, target):
    ndk = Path(os.environ["ANDROID_NDK_ROOT"])
    toolchain = ndk / "toolchains/llvm/prebuilt/linux-x86_64"
    triple = target.replace("armv7-linux-", "arm-linux-")
    system = toolchain / "sysroot/usr/lib" / triple / "26"
    platform = ("libc.so", "libm.so", "libdl.so", "libandroid.so", "libOpenSLES.so",
                "libEGL.so", "libGLESv2.so", "libmediandk.so", "liblog.so")
    providers, imports = set(), {}
    packaged = sorted(libraries.glob("*.so"))
    for library in [*packaged, *(system / name for name in platform)]:
        required = set()
        report = subprocess.check_output(
            [toolchain / "bin/llvm-readelf", "--dyn-syms", "--wide", library], text=True
        )
        for line in report.splitlines():
            fields = line.split()
            if len(fields) < 8 or not fields[0].rstrip(":").isdigit():
                continue
            symbol, _, version = fields[7].partition("@")
            key = (symbol, version.lstrip("@") or None)
            if fields[6] == "UND":
                # Weak imports may resolve to null; strong imports need a
                # provider. A successful link alone does not prove this.
                if fields[4] == "GLOBAL":
                    required.add(key)
            elif fields[4] in {"GLOBAL", "WEAK"} and fields[5] in {"DEFAULT", "PROTECTED"}:
                providers.add(key)
                if version.startswith("@"):
                    providers.add((symbol, None))
        if library in packaged:
            imports[library.name] = required
    for name, required in imports.items():
        missing = sorted(symbol + ("@" + version if version else "")
                         for symbol, version in required - providers)
        assert not missing, f"{name} has unresolved runtime symbols: {', '.join(missing)}"


def main():
    root = Path(__file__).resolve().parents[1]
    spec = importlib.util.spec_from_file_location(
        "package_android", root / ".github/scripts/package-android.py",
    )
    packaging = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(packaging)
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
extract_native_libs = false
[[package.metadata.android.application.activity]]
name = "android.app.NativeActivity"
exported = true
[package.metadata.android.signing.release]
path = "{root / 'android/keystore/debug.keystore'}"
keystore_password = "android"
'''
        )
        # Link the player as well as avcodec: the APK can contain every file
        # while libmpv still has unresolved C++ runtime symbols and cannot load.
        # A separate symbol check verifies that before reaching a device.
        # Recursive dependency staging overlaps the explicit runtime list; a
        # rebuild also exercises copies left by the previous run.
        (work / "src/lib.rs").write_text(
            '''#[link(name = "avcodec")]
unsafe extern "C" { fn avcodec_version() -> u32; }
#[link(name = "mpv")]
unsafe extern "C" { fn mpv_client_api_version() -> core::ffi::c_ulong; }
#[unsafe(no_mangle)]
pub extern "C" fn probe_version() -> u32 {
    unsafe { avcodec_version() ^ mpv_client_api_version() as u32 }
}
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
            # Release avoids the packager's debug-only no-compression shortcut,
            # exercising the manifest-driven native storage policy itself.
            print(f"Packaging read-only media inputs, attempt {attempt + 1}", flush=True)
            subprocess.run(
                ["cargo", "apk2", "build", "--lib", "--release"],
                cwd=work,
                env=env,
                check=True,
            )
            apks = list((work / "target").rglob("runtime-packaging-probe.apk"))
            assert len(apks) == 1, apks
            packaging.verify_native_layout(apks[0])
            aapt = Path(os.environ["ANDROID_HOME"]) / "build-tools/35.0.0/aapt"
            packaging.verify_native_manifest(subprocess.check_output(
                [aapt, "dump", "xmltree", apks[0], "AndroidManifest.xml"], text=True,
            ))
            with zipfile.ZipFile(apks[0]) as apk:
                entries = set(apk.namelist())
            # Old local 0.1.0 APKs used code 16777472. Even the smallest
            # version in the new epoch must upgrade those retained installs.
            aapt2 = Path(os.environ["ANDROID_HOME"]) / "build-tools/35.0.0/aapt2"
            badging = subprocess.check_output(
                [aapt2, "dump", "badging", apks[0]], text=True
            )
            code = int(re.search(r"versionCode='(\d+)'", badging)[1])
            assert code == 2 << 24 and code > 16777472, badging
            assert "versionName='0.0.0'" in badging
            for target, abi in abis.items():
                expected = {f"lib/{abi}/{p.name}" for p in sources if p.parent.name == abi}
                expected.add(f"lib/{abi}/libruntime_packaging_probe.so")
                assert expected <= entries, expected - entries
                packaged = work / f"packaged-{attempt}-{abi}"
                packaged.mkdir()
                with zipfile.ZipFile(apks[0]) as apk:
                    for source in sources:
                        if source.parent.name == abi:
                            (packaged / source.name).write_bytes(apk.read(f"lib/{abi}/{source.name}"))
                verify_runtime_symbols(packaged, target)

    assert all(hashlib.sha256(p.read_bytes()).digest() == digest for p, digest in hashes.items())
    assert all(p.stat().st_mode & 0o222 == 0 for p in sources)
    print("Both APK builds contain all three ABIs; Nix inputs remain unchanged.")


if __name__ == "__main__":
    main()

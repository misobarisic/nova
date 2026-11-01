# Release packaging

Tag builds compile the app and package its artifacts directly. PR/main checks
remain separate from the tag release graph.

## Linux AppImage tools

`.github/scripts/package-linux.sh` uses dated upstream assets, with SHA-256
verification before execution. The pins match Reader's reviewed packaging tools:

| Tool | Upstream release | SHA-256 |
| --- | --- | --- |
| linuxdeploy | `1-alpha-20251107-1` | `c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d` |
| linuxdeploy-plugin-appimage | `1-alpha-20250213-1` | `992d502a248e14ab185448ddf6f6e7d25558cb84d4623c354c3af350c25fccb3` |

Both downloads were checked against these hashes when fixing v0.0.53's checksum
failure. `continuous` assets change in place and must not be combined with a
static checksum. Dated URLs select a particular release; checksum verification
still rejects any replacement of that asset. `test_package_linux.py` exercises
the real download verifier with valid and invalid bytes and locks both pins.

## Android compilation versus assembly

The three ABI builds use `nix develop .#android`, including Rust targets, the
NDK, cargo-apk2 and native media. The separate assembly job uses
`nix develop .#android-package`: SDK build-tools 35.0.0, JDK 17 and Python only.
It needs `aapt`, `aapt2`, `zipalign` and `apksigner`; it never compiles Rust or
native libraries. Its SDK excludes platforms, the NDK, emulator and system images.
The job also removes the runner's unused preinstalled Android SDK before Nix.

v0.0.53's assembly failed before the packager started: installing the compilation
shell exhausted disk space while unpacking the NDK. The lightweight shell avoids
that redundant closure while keeping the same pinned SDK tools, payload checks,
16 KiB alignment, signing identity and universal/per-ABI compression policies.

PR/main Android checks run `.github/scripts/check-android-packaging-tools.sh` in
the lightweight shell. It rejects an SDK containing an NDK/emulator and uses the
actual tools to align, sign and verify a compiled-manifest fixture with the public
debug key. Python packaging regressions independently cover ABI merging, native
layout, manifest extraction policy, certificates and version validation.

# Native media release inputs

Nova's Android and Windows Nix shells download the four minimal mpv/FFmpeg
bundles from [nova-vendor](https://github.com/misobarisic/nova-vendor).
`release.json` pins a release, archive names and Nix content hashes.
Linux continues to use system libraries.

Native build recipes, toolchain/source locks, ABI checks, smoke probes and
[recorded sizes and validation](https://github.com/misobarisic/nova-vendor/blob/main/nix/native-media/RESULTS.md)
live in that repository. Native binaries are release assets rather than Git blobs.
The vendor release also includes the matching upstream sources and build recipe.

`default.nix` fetches each archive with `stripRoot = false`, exposes its bundle
and assembles native notices from the bundled license files. `flake.nix` maps
these outputs to the existing `NOVA_ANDROID_LIBS`, `NOVA_NATIVE_CA_BUNDLE`,
`NOVA_NATIVE_NOTICES` and `MPV_SOURCE` build inputs.

The casting packet-copy C shim compiles against the matching headers already
present in these bundles. `androidMedia/include/<abi>` exposes each ABI's
headers alongside its existing library directory; `windowsMedia/include`
exposes the Windows bundle's headers beside `64/`. `nova-remux/build.rs` selects
these paths and links the existing shared avformat/avcodec/avutil libraries.
`NOVA_FFMPEG_INCLUDE` can explicitly override the header directory. Linux uses
system FFmpeg development headers through pkg-config; the desktop Nix shell
provides them. No FFmpeg executable, second native build or transcoding encoder
is added. See [casting behavior](../../docs/casting.md).

The profile includes MKV, HEVC, AV1/dav1d, common playback codecs and subtitles,
MediaCodec on Android, D3D11 copy-back on Windows, and packet-copy remuxing.
Version 0.1.2 adds `equalizer`, `dynaudnorm`, `acompressor` and `alimiter` for the
built-in player's device-local listening controls. Bundle validation rejects
missing listening filters; the native smoke probe configures all seven effect
combinations for mono, stereo and 5.1 audio.
Android mpv links static NDK libc++/libc++abi explicitly through its C linker.
Bundle validation rejects strong runtime imports without a packaged or API-26
system provider; checking library filenames alone cannot establish loadability.
Audio/video transcoding encoders are excluded. Nova separately encodes JPEG
and lossy/lossless WebP screenshots in its Rust screenshot worker. Windows HW+
remains disabled.

To update dependencies, first publish a new nova-vendor release, update the
pins here, then check Android and Windows builds. Use `nix store prefetch-file
--unpack URL` to obtain the archive's unpacked Nix hash. Run the actual Android
packaging regression in its shell:

```sh
nix develop .#android --command python3 -B android/test_runtime_packaging.py
```

The cargo-apk2 patch in `android/cargo-apk2-readonly-libs.patch` replaces old
staged library entries before copying read-only Nix inputs. The regression
packages all three ABIs twice in release mode, checks runtime symbol providers
in the extracted media payloads and verifies the inputs remain unchanged.
`android/cargo-apk2-uncompressed-libs.patch` honors
`application.extract_native_libs = false` by storing native libraries
uncompressed, while preserving other entries' compression policy. The pinned
SDK 35 packager aligns native ZIP data to 16 KiB before signing. Both the probe
and release verifier require stored, aligned `.so` entries and the compiled
manifest's explicit direct-loading flag.

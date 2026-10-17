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

The profile includes MKV, HEVC, AV1/dav1d, common playback codecs and subtitles,
MediaCodec on Android, D3D11 copy-back on Windows, and packet-copy remuxing.
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
packages all three ABIs twice and verifies the inputs remain unchanged.

# Native media experiment results

Built on 2026-10-17 on x86_64 Linux, using the repository's Nixpkgs revision
`7a0f122f5090cf4c2ade2a13a0e229d4e19ba71f`: FFmpeg 9.0.1, mpv 0.41.0,
Android NDK 26.1.10909125/API 26, and MinGW GCC 15.3.0.

All four FFmpeg/libmpv builds and all four native smoke executables completed.
The initial experiment ran no Cargo commands. The validated runtime libraries
were subsequently integrated as Nix shell build inputs, with app linking, APK checks,
TLS roots and native notices updated. See the repository change for Rust checks.

## Recorded compilation times

Nix's successful build logs recorded these FFmpeg `buildPhase` durations:

| Target | FFmpeg compilation | mpv compilation |
| --- | ---: | ---: |
| Android ARM64 | 1m 46s | under 30s |
| Android ARMv7 | 1m 31s | under 30s |
| Android x86_64 | 2m 31s | under 30s |
| Windows x86_64 | 6m 01s | 38s |

An earlier Windows FFmpeg configuration compiled in 5m 04s. These are observed
compilation phases with up to two native builds running concurrently, using
four cores per build. They exclude configuration, other dependencies,
downloads, packaging and verification; complete clean per-target bundle times
were not measured. The initial cross-platform bring-up included build fixes
and repeated configurations, so its elapsed time is not a clean benchmark.

## Runtime library sizes

These are the sum of the stripped `.so`/`.dll` files, including the Windows
thread runtime. Headers, import libraries, certificates and notices are extra.

| Target | Runtime files | Bytes | MiB | Previous vendor MiB |
| --- | ---: | ---: | ---: | ---: |
| Android ARM64 | 7 | 14,786,048 | 14.10 | 15.36 |
| Android ARMv7 | 7 | 13,055,948 | 12.45 | 14.60 |
| Android x86_64 | 7 | 19,234,184 | 18.34 | 18.88 |
| Windows x86_64 | 8 | 25,046,324 | 23.89 | 115.22 |

The Android totals are 8.2%, 14.7% and 2.9% smaller, respectively, than the
previously vendored single libmpv files. Windows is 79.3% smaller than the
previous pinned full libmpv DLL. This is a different feature profile
and FFmpeg version, so these measurements do not establish full feature parity.

## Verification

Every bundle's build-time inspection passed:

- Correct architecture, expected shared libraries and public FFmpeg/mpv APIs.
- Every runtime dependency is bundled or an allowed platform system library.
  Windows needs neither `libstdc++-6.dll` nor `libgcc_s_seh-1.dll`.
- No Android RPATH/RUNPATH; correct Android SONAMEs and 64-bit LOAD/RELRO
  alignment for 16 KiB pages.
- PGS and dav1d enabled; PNG/MJPEG are the only actual FFmpeg encoders.
- Relocatable pkg-config modules and matching `SHA256SUMS` for every bundle.

The final Windows binaries ran under prebuilt Wine64 11.0 from an otherwise
empty directory containing only the eight bundled DLLs, the probe and fixture.
The fixture was three seconds of H.264 with B-frames and AAC in MP4.
For each of MP4, fragmented-MP4 HLS and MPEG-TS:

- The built libmpv decoded the fixture to EOF with software decoding and null
  video/audio outputs.
- The built FFmpeg API remuxed all 72 video and 142 audio packets.
- Host ffprobe confirmed strictly increasing, nonnegative decode timestamps.
- All 72 decoded video frame hashes matched the source; audio/video decoded
  without errors.
- MP4 and HLS also preserved every encoded audio/video packet hash. MPEG-TS
  changes the bitstream framing, so decoded frame hashes were compared there.

The built libraries also decoded the generated HLS input and remuxed it back
to Matroska, retaining 214 packets and identical decoded video frames. A
Matroska fixture with missing initial DTS was explicitly rejected by the final
probe, as intended.

The test outputs, logs/verification data and native probes are under the ignored
`target/native-media/` directory. The system's default Wine was 32-bit; tests
used the separate 64-bit binary. Unused Wine USB/bus/menu/managed-code components
were disabled for this host's isolated test prefix.

Nix formatting, Python syntax and `git diff --check` passed. Integration also
passed formatting, workspace Clippy with warnings denied, Linux/Windows
workspace checking, ARM64 Android library checking, 12 settings UI tests,
one decoder configuration test, three player decoder tests and all five APK
packaging unit tests.
A Windows packaging fixture verified all eight media DLLs and recursive
license/configuration files using the source-built shell's symlink layout.
The fixture uses the native probe as the executable, so it is not an app build.
All three Android ABIs linked a separate API-26 shared-library probe against
mpv and FFmpeg's JavaVM registration with no undefined symbols. The CA bundle
parsed successfully, and the built cargo-apk2 contains the runtime override.

## Boundaries and useful findings

- No Android device was connected. Android probes linked successfully, but
  device playback, GPU rendering, hardware decoding and screen-off lifecycle
  remain untested. Wine's null outputs do not validate Windows GPU/audio output.
- Matroska B-frame inputs can omit initial DTS values. A first packet-copy
  experiment reached EOF and wrote MP4 but the muxer warned about those missing
  timestamps. The final probe explicitly rejects such input; casting needs a
  timestamp reconstruction policy with a common audio/video timeline.
- The smaller Windows build uses D3D11 copy-back decoding. Direct GPU-surface
  sharing is disabled in the app UI on Windows. The omitted source types in
  README require explicit feature changes.
- Android CA roots are embedded and configured by nova-player with TLS
  verification enabled. Device HTTPS handshakes remain untested.
- The casting session's persistent upstream reader, HTTP relay, receiver
  discovery and Android foreground service are design work outside this build.

Local bundles are available at `target/native-media/{android-arm64,
android-armv7,android-x86_64,mingw-x86_64}`. Each corresponding
`nova-media-<target>.zip` contains the bundle and the locked standalone build
recipe. The app consumes these source-built outputs through its Android/Windows Nix
shells, with native notices and CA roots derived from the same dependencies.

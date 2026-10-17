# Native media vendor builds

Standalone, reproducible cross-builds of FFmpeg and libmpv on an x86_64 Linux
Nix host. Sources and toolchains come from the repository's `flake.lock`.
The Android and Windows shells import these derivations as build inputs.
Nix builds uncached dependencies and reuses unchanged outputs. The app links
and packages those outputs directly; native binaries are not committed.
Linux retains system mpv/FFmpeg.

## Build

Run from the repository root; no development shell or Cargo build is needed:

```sh
mkdir -p target/native-media
nix-build nix/native-media -A bundle-android-arm64 \
  --max-jobs 2 --cores 4 -o target/native-media/android-arm64
nix-build nix/native-media -A bundle-mingw-x86_64 \
  --max-jobs 2 --cores 4 -o target/native-media/mingw-x86_64
```

The other supported Android targets are `bundle-android-armv7` and
`bundle-android-x86_64`. `ffmpeg-*` and `mpv-*` expose individual unbundled
outputs. `probe-*` builds the small native smoke executable separately from
the runtime bundle. Nix can build native dependencies concurrently; this does
not start any Cargo commands.

Each bundle contains:

- `lib/`: stripped libmpv and the six shared FFmpeg libraries, plus Windows
  import libraries and the required MinGW thread runtime.
- `include/` and relocatable `lib/pkgconfig/`: public mpv/FFmpeg interfaces.
- `share/nova-native/`: compiler configurations, versions and source-lock
  identity; Android also includes the Mozilla CA certificate bundle.
- `share/licenses/` and `SHA256SUMS`: dependency license texts and file hashes.

Every bundle runs `inspect.py` during its build: architecture, runtime
dependencies, exported APIs, selected encoders, relocatable pkg-config files,
and 64-bit Android LOAD/RELRO alignment must pass before Nix accepts the output.

The libraries use each other's shared FFmpeg implementation. Small subtitle,
AV1 and compression dependencies are linked statically. No duplicate private
FFmpeg copy is built into mpv. The Windows C++ runtime is linked statically.

## Selected features

| Part | Selection |
| --- | --- |
| FFmpeg | 9.0.1, shared `avcodec`, `avformat`, `avfilter`, `avutil`, `swscale`, `swresample` |
| libmpv | 0.41.0 with GPL components, embeddable OpenGL render API, no command-line player |
| Android | NDK 26.1.10909125, API 26, ARM64 / ARMv7 / x86_64, EGL, OpenSL ES, MediaCodec |
| Windows | MinGW x86_64, WASAPI, embedded OpenGL, D3D11 hardware decoding with copy-back |
| Video | H.264, HEVC, AV1 (dav1d), VP8/9, MPEG-1/2/4, VC-1/WMV3, Theora and still-image decoding |
| Audio | AAC, AC-3/E-AC-3, DTS, MP2/3, FLAC, ALAC, Opus, Vorbis, WMA and common PCM formats |
| Subtitles | libass with FreeType, HarfBuzz shaping and FriBidi; text and bitmap decoders, including PGS |
| Inputs | Common movie/audio containers, HLS, HTTP(S), local files, pipes and file descriptors |
| Remux outputs | MP4/MOV, fragmented-MP4 HLS, MPEG-TS, Matroska, WebVTT |
| Encoders | PNG and MJPEG for screenshots only; no audio/video transcoding encoders |
| TLS | Android: static mbed TLS 3.6.7. Windows: system Schannel |

`--disable-everything` and explicit FFmpeg component lists keep the build
bounded. FFmpeg enables some supporting components transitively. mpv uses
`--auto-features=disabled` with explicit playback features. There are no native
test suites, tools, demos, scripting engines, optical-disc libraries, Vulkan
loader/shader compilers or font-subsetting libraries in the build.
libplacebo remains a required mpv dependency, built statically without GPU
backends; mpv's existing OpenGL renderer uses its shader and color helpers.
Vulkan headers and fast_float are build-time headers only.

64-bit Android libraries pass both `max-page-size=16384` and
`common-page-size=16384` to the linker, following the
[NDK r27-and-earlier guidance](https://developer.android.com/guide/practices/page-sizes#compile-r27).
The C++ runtime is static, avoiding a separate r26 `libc++_shared.so`.

## Smoke probe

`smoke.c` checks that the selected muxers/decoders exist and that no unexpected
encoders were enabled. It then decodes the input to EOF using headless mpv and
remuxes its timestamped audio/video packets through the public FFmpeg API. It accepts:

```text
smoke INPUT OUTPUT FORMAT
```

For Windows, build `probe-mingw-x86_64`, copy its `bin/smoke.exe` and the
bundle's DLLs into an empty working directory, and run it under Windows or
Wine. A suitable fixture can be generated with an existing host FFmpeg:

```sh
ffmpeg -f lavfi -i testsrc2=size=320x180:rate=24 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 3 \
  -c:v libx264 -g 24 -bf 2 -pix_fmt yuv420p -c:a aac input.mp4
wine ./smoke.exe input.mp4 output.mp4 mp4
wine ./smoke.exe input.mp4 playlist.m3u8 hls
wine ./smoke.exe input.mp4 output.ts mpegts
```

Use host `ffprobe` and `ffmpeg -v error -i OUTPUT -f null -` to verify the
outputs' streams, timestamps and decoding. The probe's headless playback does
not exercise the app's GPU render context, hardware decoding or Android
lifecycle. Android probes are cross-compiled for device-side testing.

## Integration boundaries

- Android HTTPS must set mpv's `tls-ca-file` / FFmpeg's `ca_file` to the
  packaged `cacert.pem`. `nova-player` embeds the built roots, refreshes
  its private cache copy and sets `tls-ca-file` with verification enabled.
  mbed TLS does not
  automatically use Android's trust store.
- This deliberately smaller profile excludes DASH/libxml2, RTSP/RTMP,
  optical-disc inputs and uncommon codecs. Additional source types need
  corresponding feature changes. No cast implementation is added.
- Windows direct GPU-surface sharing through ANGLE/CUDA/DXVA2 is outside this
  profile; D3D11 copy-back decoding remains available. Standalone D3D11 video
  output would pull in shaderc and SPIRV-Cross and is unused by the embedded
  OpenGL render API.
- Remux support does not make a codec playable by a receiver. The casting
  layer still needs receiver capability checks, stream selection, timestamp
  handling, cancellation and a continuous upstream reader.
  Matroska B-frame inputs can lack initial DTS values; the probe explicitly
  rejects missing timestamps. The production remuxer must reconstruct them
  rather than rely on the muxer guessing or independently shifting tracks.
- The app links and packages the built libraries; Android packaging checks
  all seven media libraries per ABI. Windows packages DLLs and native notices.
  TLS roots and the in-app source/license catalog are included. Native Windows
  GPU/audio and Android device playback still need physical-device validation.
- Linux will use system FFmpeg libraries when the casting backend is added.

See `RESULTS.md` for the builds and checks performed in this workspace.

# Third-party notices

Nova's original code and all Nova workspace crates are licensed under
GPL-3.0-or-later; see [LICENSE](LICENSE). The app's **Settings → About** page
contains the full license catalog for the build: every runtime Rust crate
selected for that target and feature set, each license text, clickable
upstream source links, all Nova workspace crates, and the native vendors below.
The Cargo report is generated from [Cargo.lock](Cargo.lock) with cargo-about;
unknown or unresolved licenses stop the build rather than being silently
omitted.

## Android native media library

The APK includes `libmpv.so` for `arm64-v8a` and `x86_64`, built from the
`full` flavor of [media-kit/libmpv-android-video-build v1.1.11](https://github.com/media-kit/libmpv-android-video-build/releases/tag/v1.1.11).
Binary hashes, source archive URLs, and extraction/build provenance are in
[`vendor/android-libs/SOURCES`](vendor/android-libs/SOURCES). The build sets mpv
`-Dgpl=false` and FFmpeg `--disable-gpl --disable-nonfree --enable-version3`.

| Component | Pinned version | License | Upstream |
|---|---:|---|---|
| mpv | `78d43740f52db817d98bcf24fb30a76ab6fa13ff` | LGPL-2.1-or-later | [mpv](https://github.com/mpv-player/mpv/tree/78d43740f52db817d98bcf24fb30a76ab6fa13ff) |
| FFmpeg | 6.0 | LGPL-3.0-or-later | [FFmpeg](https://github.com/FFmpeg/FFmpeg/tree/n6.0) |
| libass | 0.17.1 | ISC | [libass](https://github.com/libass/libass/tree/0.17.1) |
| FreeType | 2-13-0 | FTL | [FreeType](https://gitlab.freedesktop.org/freetype/freetype/-/tree/VER-2-13-0) |
| FriBidi | 1.0.12 | LGPL-2.1-or-later | [FriBidi](https://github.com/fribidi/fribidi/tree/v1.0.12) |
| HarfBuzz | 7.2.0 | MIT | [HarfBuzz](https://github.com/harfbuzz/harfbuzz/tree/7.2.0) |
| mbed TLS | 3.4.0 | Apache-2.0 | [mbed TLS](https://github.com/Mbed-TLS/mbedtls/tree/v3.4.0) |
| dav1d | 1.2.0 | BSD-2-Clause | [dav1d](https://code.videolan.org/videolan/dav1d/-/tree/1.2.0) |
| libxml2 | 2.10.3 | MIT | [libxml2](https://gitlab.gnome.org/GNOME/libxml2/-/tree/v2.10.3) |

The build recipe itself is MIT-licensed. Its full notice, along with the full
standard license texts for these native components, is bundled into the About
catalog. The unused `libmediakitandroidhelper.so` is not included. The only
`NEEDED` libraries recorded for the bundled `libmpv.so` are Android system
libraries: `libm`, `libandroid`, `libOpenSLES`, `libEGL`, `libdl`, and
`libc`.

## Other bundled and system-provided components

- `assets/fonts/Roboto-Regular.ttf` is licensed under Apache-2.0; the full
  font license file is embedded in the About catalog.
- Linux links to system/Nix-provided `libmpv` and X11 libraries; Nova does
  not bundle a Linux `libmpv.so`. The distro's package notices apply.
- Android's system libraries are supplied by the OS and are not copied into
  the APK by Nova.

## AniKoto provider behavior

The bundled AniKoto provider adapts request, VRF transformation, MegaPlay
decryption/token signing, and Mewcdn extraction behavior
from the [AniKoto extension in `yuzono/anime-extensions`](https://github.com/yuzono/anime-extensions).
The upstream extension is licensed under Apache-2.0; its license is available
at <https://github.com/yuzono/anime-extensions/blob/master/LICENSE>. Nova's
JavaScript source is an independent port and does not include the extension's
Kotlin source. See [`crates/providers/plugins/anikoto/NOTICE.md`](crates/providers/plugins/anikoto/NOTICE.md).

The checked-in source inventory and hashes are maintained in
[`assets/open_source_project_sources.tsv`](assets/open_source_project_sources.tsv),
[`assets/open_source_vendors.txt`](assets/open_source_vendors.txt), and
[`vendor/android-libs/SOURCES`](vendor/android-libs/SOURCES). Re-audit these when the
Cargo graph, native release/flavor/build options, or bundled assets change.

# Third-party notices

Nova's original code and all Nova workspace crates are licensed under
GPL-3.0-or-later; see [LICENSE](LICENSE). The app's **Settings → About** page
contains the full license catalog for the build: every runtime Rust crate
selected for that target and feature set, each license text, clickable
upstream source links, all Nova workspace crates, and the native vendors below.
The Cargo report is generated from [Cargo.lock](Cargo.lock) with cargo-about;
unknown or unresolved licenses stop the build rather than being silently
omitted.

## Android and Windows native media

Current builds use the immutable [nova-vendor v0.1.1 bundles](https://github.com/misobarisic/nova-vendor/releases/tag/v0.1.1),
pinned with archive hashes in [nix/native-media/release.json](nix/native-media/release.json).
Android supports arm64-v8a, armeabi-v7a and x86_64; Windows uses x86_64 MinGW.
These bundles contain mpv 0.41.0, FFmpeg 9.0.1 and their selected dependencies.
mpv is GPL-2.0-or-later; FFmpeg is LGPL-2.1-or-later on Windows and
LGPL-3.0-or-later on Android. The component inventory and source links are in
[assets/open_source_project_sources.tsv](assets/open_source_project_sources.tsv).

Each bundle supplies per-component licenses, source/configuration metadata and
SHA256SUMS. The target's full native notices are embedded into About from those
immutable files; Windows packaging also includes them beside the executable.
The packet-copy casting wrapper links the same shared FFmpeg libraries and
compiles against their matching headers. It adds no separate FFmpeg binary or
audio/video encoder. See [native media inputs](nix/native-media/README.md) for
the build recipe, provenance and validation process.

## Casting transport

Google Cast uses oxicast 0.0.3 under MIT OR Apache-2.0. The original licenses and
source are retained in [vendor/oxicast](vendor/oxicast); its
[patch record](vendor/oxicast/NOVA_PATCH.md) pins upstream commit
`99e41cb1531a43190713e9a3f4ad50ac52c79b96` and explains the narrow IPv6 and
diagnostic changes. DLNA control lives in Nova's GPL-3.0-or-later casting crate.

## Other bundled and system-provided components

- `assets/fonts/Roboto-Regular.ttf` is licensed under Apache-2.0; the full
  font license file is embedded in the About catalog.
- Linux links to system/Nix-provided `libmpv`, FFmpeg and X11 libraries; Nova does
  not bundle a Linux `libmpv.so`. The distro's package notices apply.
- Android's system libraries are supplied by the OS and are not copied into
  the APK by Nova.

## AniKoto provider behavior

The AniKoto test fixture adapts request, VRF transformation, MegaPlay
decryption/token signing, and Mewcdn extraction behavior
from the [AniKoto extension in `yuzono/anime-extensions`](https://github.com/yuzono/anime-extensions).
The upstream extension is licensed under Apache-2.0; its license is available
at <https://github.com/yuzono/anime-extensions/blob/master/LICENSE>. Nova's
JavaScript source is an independent port and does not include the extension's
Kotlin source. Production AniKoto packages are maintained in the separate `nova-addons`
repository and carry their own notices. See the retained test attribution in
[`crates/providers/test-fixtures/addons/anikoto/NOTICE.md`](crates/providers/test-fixtures/addons/anikoto/NOTICE.md).

## Re:ANIME / FlixCloud playback behavior

The bounded HLS relay in `crates/providers/src/hls_proxy.rs` independently ports
playlist rewriting and PNG/WebP-wrapped XOR segment decoding behavior from the
[ReAnime extension in `yuzono/anime-extensions`](https://github.com/yuzono/anime-extensions/tree/master/src/en/reanime),
licensed under Apache-2.0. No upstream Kotlin source is bundled. The production
Re:ANIME JavaScript package lives in `nova-addons` and carries its own full
license and attribution. Playback uses the upstream extension's external
`enc-dec.app` token/playlist decoding service.

The checked-in source inventory and hashes are maintained in
[`assets/open_source_project_sources.tsv`](assets/open_source_project_sources.tsv),
[`assets/open_source_vendors.txt`](assets/open_source_vendors.txt), and
the pinned [native release inputs](nix/native-media/release.json). Re-audit these when the
Cargo graph, native release/flavor/build options, or bundled assets change.

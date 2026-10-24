# Repository follow-ons

These are planned improvements; they are not implemented by this document
update.

## Android screenshot subtitle backgrounds in HW and SW

Status: deferred at the user's request on 2026-10-13; current capture behavior
remains unchanged.

The user confirmed that HW+ screenshots work on the S25 FE, but HW and SW
captures add opaque black rectangles behind subtitles. The supplied comparison
shows outlined text over the video in HW+ and white text on solid black panels
in the other two modes. Reproduce with the same video, subtitle track and
position before attributing the difference to a particular rendering bug.

`crates/player/src/screenshot.rs` routes Android HW+ through the private GPU
target in `gpu_screenshot.rs`; HW and SW still use mpv's asynchronous
`screenshot-to-file` command with the `subtitles` flag. The different subtitle
composition paths are the first place to investigate. Consider using the
existing GPU capture path for all Android decoder modes, preserving the
selected decoder, subtitle preferences, pause state and playback position.
Do not enable mpv's advanced render control without satisfying its threading
contract.

Validation should compare HW+, HW and SW on the device, with outlined text
subtitles over a bright or colored video background. Extend the real-mpv/EGL
regression in `gpu_screenshot.rs` to check transparent gaps inside subtitle
glyph bounds; its current black background can hide an opaque black panel.
Retain coverage for native display dimensions, rotation, paused capture,
GL-state restoration, timeout/cancellation and stale playback sessions. Verify
that screenshots exclude Nova's OSD and preserve intentionally configured
subtitle backgrounds as well as outlines and transparency.

Acceptance: HW and SW screenshots match the displayed subtitle styling without
unrequested black panels, and HW+ capture continues to work. Any implementation
needs an ARM64 release build and device verification. Current builds retain the
separate HW+ GPU and HW/SW mpv capture paths until this follow-on is implemented.

## Stream cache persistence and scheduled cleanup

Implemented for Supernova: validated stream lists, discovery and normalized
metadata now persist in the OS cache directory with TTL/LRU bounds. A worker
checks a saved daily cleanup deadline every two minutes and on wake; due passes
prune the disk cache, while reads enforce expiry immediately. Unchanged cache
snapshots skip file writes. See `src/app/plugin_cache.rs` and the provider guide. TorBox still
checks availability and requests a fresh link on selection.

The shared next-episode prefetch cache remains in memory for five minutes.
Ordinary remote addon stream URLs remain memory-only unless a safe refresh
contract is introduced. Expiring playback URLs, authorization headers and API
keys must never enter persisted stream caches.

## UI thread audit

Reviewed on 2026-10-20. Explicit episode/season/series watched edits now patch
visible Detail and loaded queue rows immediately, with coalesced Library/Home
derivation on a profile-owned worker. Downloads skips unchanged timer
projections, shares typed episode metadata across labels, and claims automatic
watched removals before doing persistence and file cleanup on a worker.

The local debug `measure_season_watch_action_callback_cost` fixture in
`src/app/detail.rs` contains 60 shows and 6,000 progress records. Six callback
samples fell from 203–206ms to 6.3–9.4ms; the later UI publication took 2.7ms
and was ready after about 1.19s of background preparation.
These are fixture timings, not Android frame-rate or device measurements.

Remaining candidates need profiling and their own changes:

- `calendar.rs::apply_calendar_to_ui` scans matching Library episode caches on
  date/search/navigation changes, then visible `home.rs::upcoming_row` calls
  reread title metadata. Start with per-publication typed metadata reuse and
  retain a revision-checked, display-ready schedule before moving the join off
  the UI thread.
- `viewing_journal.rs::JournalTracker::observe` can load the full session archive
  on its first playback sample. Finalization also reads/serializes/writes while
  holding Shared and the journal writer lock. Preload and prepare checkpoints
  off-thread, preserving ordered durability and explicit stop/pause/shutdown
  flush barriers; replacing the final write with an unguarded spawn would lose
  that guarantee.
- `sync_inspector.rs` joins episode aliases and progress and sorts the results
  before truncating the active inspector to 80 rows, including search and its
  two-second refresh. Prepare this from a bounded worker snapshot with profile,
  query and data-revision guards, keeping sync Store lock holds short.

Home/Library navigation now retains same-profile models and requests the shared
progress projection worker instead of scanning metadata in the navigation callback.
Library prefetch eligibility and scheduled episode-refresh preparation run on
profile-owned workers; cold Home catalog-cache decoding also runs off-thread.
Startup restoration retains its synchronous bootstrap. See
`src/app/progress_projection.rs` and the responsiveness quick reference in
`docs/project-structure.md`.

## Home identity and action audit

Reviewed on 2026-10-24. Watch history now collapses proven episode aliases,
shares hidden clocks and visible Continue exclusions, and keeps its proof on
the progress worker. Continue resolves partial/rewatch alias targets to a
unique cached native episode; disabled or capped-out Continue cards no longer
suppress New Episodes.

The remaining audit findings were fixed in the next change:

- Continue/New dismissals retain the typed source and proven global episode
  identities in the existing hidden maps, with legacy raw-ID reads. Other
  sources cannot restore a dismissed episode/batch, while different media
  types, unrelated seasons, later releases and newer playback remain eligible.
- `episode_home_enter` matches both media type and raw ID for Continue, New
  Episodes and Upcoming, and returns without reusing a previous Detail target
  when the requested typed entry is absent.
- Continue/New touch sheets and native popups capture the painted typed
  episode. Their `home_episode_action` callback revalidates that identity after
  model updates; a reordered index cannot retarget the open menu.

Focused backend and headless menu regressions cover these contracts. No
Android build or device verification was requested for this follow-up.

## Stored-data compression audit

Status: measured on 2026-10-14. Library/history snapshots and their baselines now
use independently encoded authoritative rows and an ordered background save
worker; the other compression candidates below remain follow-ons. The audit
could not open the local redb database read-only without recovery and left it
untouched. This does not establish corruption or the sizes of its logical data.
The provider file and new usage index were absent. DB-family results below use
seeded, model-shaped fixtures; the artwork-index sample reconstructs 637 actual
variant headers in memory, without saving or printing their contents.

Benchmarks imported `crates/storage/src/codec.rs` into an optimized standalone
Rust runner in default Nix: current adaptive zstd level 3, checksums and envelope,
three warmups and 31 timed rounds, exact round-trip checks, median milliseconds.
Hardware: AMD Ryzen 5 5500U, not CPU-pinned. Times include codec allocations and
copies, but exclude JSON serialization, disk IO and application transactions.
These are desktop codec timings, not Android or end-to-end checkpoint timings.

| Sample | JSON bytes | Encoded bytes | Saved | Encode / decode ms |
|---|---:|---:|---:|---:|
| Home: 8 catalogs, 160 previews (fixture) | 192,450 | 54,654 | 71.60% | 0.771 / 0.193 |
| Provider: 200 entries, 4,000 previews (fixture) | 4,941,415 | 602,760 | 87.80% | 11.899 / 4.826 |
| Library: 1,000 titles (fixture) | 1,048,763 | 291,907 | 72.17% | 4.504 / 1.172 |
| History: 10,000 episodes (fixture) | 2,158,829 | 278,799 | 87.09% | 5.233 / 2.429 |
| Library baseline: 1,000 titles (fixture) | 1,106,801 | 298,151 | 73.06% | 4.731 / 1.352 |
| Progress baseline: 10,000 episodes (fixture) | 2,378,829 | 288,185 | 87.89% | 5.471 / 3.294 |
| Tracking: 200 bindings (fixture) | 895,539 | 36,203 | 95.96% | 0.965 / 0.584 |
| Downloads: 500 jobs (fixture) | 366,043 | 74,007 | 79.78% | 1.024 / 0.293 |
| Usage index: 637 actual headers, reconstructed | 232,998 | 25,380 | 89.11% | 0.376 / 0.135 |

Fixtures used Python `random.Random(20261014)`, varied titles, independently
generated IDs/URL paths/timestamps, 32 artwork hosts, mixed genres and distinct
45–100-word descriptions from ordinary words plus 1,200 generated proper-name
words. Provider pages overlap among 1,000 titles; baselines contain per-item JSON
strings. None of these fixtures measures actual DB-family usage.

Recommended implementation order:

1. Add `home:catalog-rows:v1` to `src/app/io.rs::metadata_cache_key`, extending its
   policy regression. It stores the same cache shape as Home showcase, already
   uses `write_json`, and can migrate atomically on its next metadata write.
2. Compress the disposable provider cache file with versioned framing, legacy
   JSON reads and atomic replacement. Preserve its 121 MiB decoded bound or raw
   fallback: the current storage codec caps values at 64 MiB. Whole-file encoding
   currently occurs while the provider service mutex is held; measure request
   latency as well as disk savings.
3. Implemented for Library/history: generic registered collections share the
   existing `try_write_batch` transaction with scalar bookkeeping and sync rows.
   `state_rows_v1` stores individual title/episode/baseline payloads, retaining
   encoded bytes for unchanged records. Logical JSON reads stay compatible;
   descriptors fail old typed readers closed, and corrupt payloads block updates.
   Normal app saves serialize/compress on an ordered worker, with lifecycle and
   remote-apply flush barriers. The existing 1 KiB / 10% compression policy is
   unchanged, so tiny progress rows stay raw. Whole-blob benchmark savings above
   do not predict per-record savings. Additional authoritative collections remain
   deferred; there is no sync wire or ALPN change.
4. Normalize the usage index before adding a dependency to `nova-media`: replacing
   its duplicated URL-bearing key with recipe fields reduced the reconstructed
   plain JSON to 174,235 bytes (25.22% smaller). Preserve reconstruction and clean
   shutdown markers. Consider compression after measuring larger real workloads.
5. Defer tracking/download compression until actual sizes warrant it; their
   recovery-critical state needs the same authoritative storage contract.

The current 1 KiB threshold left all 10,000 modeled progress sync records raw:
675–980 bytes each, including escaped progress JSON, `__nova_progress_v1`
candidates and versions. Encoding added 100,000 envelope bytes. An isolated
temporary-codec probe with a 256-byte threshold reduced their combined
8,858,313 bytes to 2,927,960 (66.95%), but median batch encode/decode took
153.431 / 26.650 ms. Ordinary saves touch dirty rows, while full-store migration
and startup pay the batch cost. Evaluate a separate row policy rather than
globally lowering metadata thresholds.

Normal Library/history saves now run on the persistence worker; lifecycle and
remote-projection flushes still wait for durable completion. A follow-on
benchmark should measure whole transactions and Android frame/flush latency,
rather than extrapolating these whole-blob codec timings to per-record saves.
Tiny settings, counters, maintenance markers, clocks, credentials and secrets
should remain plain by default. Compression does not reduce retained decoded
RAM or immediately shrink existing redb allocation. All audit runners/fixtures
were removed; the audit did not modify production data. The implemented
authoritative row policy is described in `docs/project-structure.md`.

## Android video-restoration latency

Status: planned, not implemented. The restoration fix in `62ea798`
(`fix(android): restore video after window recreation`) works on the user's
S25, but the user reports that the picture takes roughly one to two seconds
to return. Keep that working reload-based recovery and reduce avoidable
scheduling and blocking delays before exploring recovery without a reload.

### Current behavior and investigation

The player state timer in `src/app/run.rs` runs every 250 ms. Recovery in
`crates/player/src/lib.rs` waits for that timer to consume the readiness gates
and queue a reload. This adds up to one timer interval under normal scheduling;
it does not explain the entire reported delay.

Recovery also reads playback properties synchronously during rendering teardown
and prepares/submits loads synchronously during `BeforeRendering`.
[mpv's render API](https://raw.githubusercontent.com/mpv-player/mpv/master/include/mpv/render.h)
warns that ordinary synchronous client calls on the rendering thread can cause
waits and timeouts. This is a possible contributor, not a confirmed diagnosis.
Measure those stages separately from source reopening and decoder startup.

### Implementation approach

- Trigger the existing session-scoped recovery scheduler immediately when resume
  or renderer setup completes the foreground/renderer readiness gates. Keep the
  timer as a fallback. Both paths must consume the same recovery request so
  duplicate events cannot produce duplicate reloads.
- Replace synchronous teardown reads with cached `time-pos` and pause
  observations. Preserve explicit seeks and pause changes, and retain the last
  valid snapshot through missing playback properties.
- Make Android load preparation nonblocking: asynchronously set source headers,
  external subtitles, and pause, then submit the existing named `loadfile`
  command after preparation succeeds. The bundled older mpv supports the
  asynchronous APIs; see the
  [mpv 0.37 client API](https://raw.githubusercontent.com/mpv-player/mpv/v0.37.0/libmpv/client.h).
  Retain named arguments rather than version-dependent positional start options.
- Use a wakeup callback only to schedule Slint event-loop processing. Drain mpv
  events without waiting there, and associate preparation/command replies with
  playback-session and request IDs. Handle immediate submission failures and
  later asynchronous errors as session-local failures.
- Recheck session identity and renderer readiness before advancing preparation
  or submitting a load. Close or stream replacement invalidates old work;
  temporary window loss retains recovery until readiness returns. New playback
  must apply its own options before loading so stale preparation cannot leak
  headers, subtitles, or pause state into it.
- Keep original-context render teardown, selected decoder, position, pause
  state, and the current pause-on-background behavior. No settings, storage,
  public API, or wire-protocol changes are intended. Update the Android behavior/navigation
  documentation with the eventual implementation.

### Timing and regression coverage

Record monotonic timings for resume, teardown, renderer readiness, preparation,
load submission/completion, and the first render after a frame-ready
notification. Include session/request IDs, never source URLs or headers. A
render call alone is not proof of moving video or a restored paused frame;
confirm those on the device.

Extend player tests for immediate scheduling in both event orders, duplicate
timer/lifecycle triggers, preparation ordering, asynchronous failures, missing
properties, paused restoration, and close/replacement/window loss during
preparation. Preserve the existing initial-loading and same-URL replacement
coverage. Run formatting, workspace Clippy, desktop checks, relevant player
tests, sync tests, and the repository's required workspace/UI tests. Classify
existing unrelated failures separately rather than weakening their assertions.

Compare ten recovery cycles against `62ea798` on the S25 using the same source
and decoder for each comparison. Cover HW+, HW, software decoding, paused
playback, rotation, initial loading, app switching, and lock/unlock. Verify
position/pause preservation, responsive controls, correct old-context teardown,
and exactly one reload per interruption. Compare stage timings and median
time to restored picture; distinguish source/decoder startup from scheduler
and client-call waits.

### Acceptance and delivery

Recovery should start as soon as both readiness gates are satisfied, without
waiting for the periodic timer. Rendering callbacks should perform no
synchronous recovery property reads or load commands. Device measurements must
establish whether the change improves restoration; do not promise subsecond
restoration while reopening the source and decoder remains necessary.

For the eventual implementation, build an ARM64 release in
`nix develop .#android` and send a distinctly named APK to the user's S25 over
Tailscale, following the delivery request from this session. If physical-device
automation is unavailable, report device measurements as pending. Preserve
unrelated workspace changes; do not automatically commit, push, or publish.
This document itself does not implement, build, or send a new APK.

## Android last-frame cover during restoration

Status: investigated, proposed; no runtime implementation. This complements
the latency work above: a still image can cover the black interval while the
existing recovery reloads and preloads video. It does not shorten source or
decoder startup, and it must preserve the selected return behavior (stay paused
with controls visible, or auto-continue).

### Findings and constraints

`crates/player/src/lib.rs` renders mpv into the window's default framebuffer in
`BeforeRendering`, before Slint paints the controls. It currently retains no
video image. The local Android backend patch suspends Slint on
`TerminateWindow`; Slint invokes `RenderingTeardown` with the original surface
active, then destroys it. This is the last opportunity to download an owned GPU
copy while its context is valid.

Keeping the old mpv context or a texture handle for the new renderer is unsafe:
[mpv requires the original GL context](https://raw.githubusercontent.com/mpv-player/mpv/v0.37.0/libmpv/render.h).
The surviving image must own CPU pixels, so Slint can upload them in the
replacement context. Reading framebuffer 0 only at teardown is unreliable:
[EGL does not guarantee its color-buffer contents after swapping](https://raw.githubusercontent.com/KhronosGroup/EGL-Registry/main/sdk/docs/man/html/eglSwapBuffers.xhtml)
unless buffer preservation is enabled. Making the old context current does
not recover discarded pixels.

An asynchronous `screenshot-raw` on activity pause is a smaller alternative to
prototype, but not a reliable sole capture path. It can race window termination,
and [mpv's screenshot path may fall back to downloading a hardware frame](https://raw.githubusercontent.com/mpv-player/mpv/v0.37.0/player/screenshot.c).
In [mpv 0.37's libmpv output](https://raw.githubusercontent.com/mpv-player/mpv/v0.37.0/video/out/vo_libmpv.c),
GPU screenshots require advanced render control, which Nova does not enable.
Do not enable that mode without first satisfying its threading/event-processing
contract. In particular, do not run or wait for screenshot client commands in
rendering callbacks. HW+ capture needs explicit device verification.

### Preferred prototype

- Keep a bounded, owned GPU copy of the most recently rendered video before
  Slint composites its OSD. Copy/downscale into an offscreen target while the
  window pixels are valid, rather than reading the swapped window later.
  Prototype a maximum of 1280 × 720 RGBA pixels (about 3.5 MiB per copy), adapted
  to the content aspect ratio. This adds GPU work during playback; measure its
  cost before adopting it. Avoid CPU readback on every frame.
- During teardown, download that retained copy once while the original context
  is current, then release all old GPU resources and the mpv context there.
  Restore all touched GL state, including pixel-pack buffer/row settings if used;
  the current state guard already covers pack alignment but not those bindings.
  Never delay native-window destruction waiting for a later redraw or worker.
  Context loss or capture failure must fall back to existing recovery.
- Keep the CPU image only in memory and associate it with the playback session,
  recovery generation, position, and geometry. A Slint image in `PlayerOverlay`,
  wired through `AppWindow`, can cover the mpv underlay beneath live controls,
  menus, and next-episode offers. Do not capture Nova's OSD into the image.
  Preserve mpv subtitles and fit/crop behavior; retain sufficient geometry to
  avoid stretching or double letterboxing across rotation. Release the image
  on close, stream replacement (including the same URL), or invalidating seeks.
- Continue the real reload/preload while the cover is visible. Remove it only
  after the replacement renderer draws valid video for the current recovery.
  Combine the reload generation and video-ready observations with render API
  update/frame-info signals and a successful render. A setup callback, duration,
  position, update callback, or successful render alone does not establish that
  decoded video has replaced the blank startup frame. Verify this handoff for
  paused playback too; it must not depend on the playback clock advancing.
  On reload failure, expose the existing error and controls instead of leaving
  an apparently restored still image indefinitely.

### Validation before implementation is accepted

Add session-policy tests for capture/handoff ordering, duplicate lifecycle
events, close/replacement/seek, failed capture/reload, and a paused first frame.
Add headless UI coverage for layering, controls, and portrait/landscape fit.
On the S25, test HW+, HW, and software decoding with app switching, lock/unlock,
rotation without a preceding pause, initial loading, and both return settings.
Initial loading with no decoded frame should use the normal loading UI.
Measure GPU-copy cost during playback, readback time during teardown, peak
memory, time to the cover appearing, and time to real video. Confirm no flash,
stale episode, duplicated controls, extra reload, or briefly playing paused
video. Device measurements remain pending; no new APK was built for this note.

## Release signing

The tag workflow builds release APKs, while Cargo's release-signing metadata
currently points to the tracked `android/keystore/debug.keystore` with the public
`android` password. Before distributing a production release:

- create a private release keystore and store its path/password in GitHub
  Actions secrets;
- configure the release workflow to use those secrets while keeping the
  debug key for local development; and
- plan the signing-certificate transition: APKs signed with a new key generally
  cannot update installs signed with the existing key, so users may need to
  reinstall unless a supported key-rotation path is arranged.

## Noto Color Emoji subsetting

The full-font format replacement is implemented: `assets/fonts/NotoColorEmoji.ttf`
uses upstream's full COLRv1 vector artwork, with compatibility aliases preserving
the previous bitmap font's 1,510 cmap entries and 4,176 GSUB input sequences.
No character or vector-glyph subsetting is applied. The generated asset is
5,060,792 bytes, down from the full bitmap font's 10,730,124 bytes (52.8% before
package compression). `assets/fonts/README.md` records input/output hashes and
reproduction commands; `assets/fonts/tools/prepare_emoji.py` verifies the full
baseline repertoire and `emoji-coverage.txt` drives Slint shaping coverage
without host fonts. Desktop's pinned FemtoVG fork painter reuses skrifa 0.44 and
tiny-skia 0.12; Android keeps native Skia/FreeType COLRv1 rendering. Production fallback now orders prepared Roboto, Noto, then system families; only six overlapping emoji cmap mappings are removed from UI Roboto, preserving standalone digits and routing complete keycaps to Noto. The full 4,176-sequence test asserts the actual Noto source through this production fallback. Android subtitles retain an unmodified Roboto asset because libass has no system-font fallback.

Remaining validation and optional future work:

- Verify actual color rendering on the S25 (representative addon text, flags,
  joined emoji and skin tones). The configured ARM64 Skia archive contains
  COLRv1 traversal/paint support, but that is not a physical-device visual test.
- Record signed release APK sizes separately from uncompressed font bytes;
  compression and CPU painter dependencies affect the final package delta.
- Any future subset must preserve arbitrary addon text coverage and substitution
  dependencies. Do not infer a safe repertoire from emoji literals in Nova's
  source. Preserve font licensing and reproducible provenance.
- Run only one Cargo command at a time; retain only the intended packaged asset
  and reproducible tooling, with no tracked bitmap baseline or scratch files.

## APK license and source delivery

`About → Licenses` and `THIRD_PARTY_NOTICES.md` now include the build's Rust
dependency license texts and source links, the native media vendor inventory,
and the bundled font license. That satisfies the in-app notice/catalog part;
it does not deliver the corresponding source for a particular APK.

Before publishing, update the release workflow to attach or link the exact
matching Nova source revision and the corresponding native media source/build
materials for each APK, with ABI and hash provenance. Review applicable
GPL/LGPL distribution requirements for the shipped combination, including
any installation information that applies. The current release workflow only
attaches APKs and GitHub-generated notes.

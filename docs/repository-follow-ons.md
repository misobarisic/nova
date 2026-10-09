# Repository follow-ons

These are planned improvements; they are not implemented by this document
update.

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

## APK license and source delivery

`Settings → About` and `THIRD_PARTY_NOTICES.md` now include the build's Rust
dependency license texts and source links, the native media vendor inventory,
and the bundled font license. That satisfies the in-app notice/catalog part;
it does not deliver the corresponding source for a particular APK.

Before publishing, update the release workflow to attach or link the exact
matching Nova source revision and the corresponding native media source/build
materials for each APK, with ABI and hash provenance. Review applicable
GPL/LGPL distribution requirements for the shipped combination, including
any installation information that applies. The current release workflow only
attaches APKs and GitHub-generated notes.

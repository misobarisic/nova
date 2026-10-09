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
  state, and background-audio behavior. No settings, storage, public API, or
  wire-protocol changes are intended. Update the Android behavior/navigation
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

## Pull-request validation

The current Android pull-request job checks the workspace and builds the
Android target, but it does not run the README's full test gate or formatting
checks. Add a validation job for:

- `cargo fmt --all -- --check`;
- `cargo test --workspace --locked`; and
- optionally `cargo clippy --workspace --all-targets --locked` after the
  existing lint baseline has been reviewed.

Keep the Android APK build as a separate check so failures identify the
platform-specific cause.

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

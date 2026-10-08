# Nova's Slint Android lifecycle patch

Vendored from the crates.io `i-slint-backend-android-activity` **1.18.0**
package, upstream Slint commit `bd20dab8529add087b5cbc81aec70bf30861ae4c`,
directory `internal/backends/android-activity`. Upstream source and licensing
notices are retained; the normalized registry manifest is used unchanged.

Only `androidwindowadapter.rs` is patched:

- Handle `TerminateWindow` by disabling drawing and calling `SkiaRenderer::suspend()`
  before the event returns, while android-activity still owns the valid old native
  window. Skia invokes `RenderingTeardown` with the original GL surface current.
- Enable drawing and request a fresh frame after `InitWindow` successfully
  associates the renderer with the new window.
- Suppress `do_render()` between window termination and successful initialization,
  including queued redraws and animation wakes.

The root `[patch.crates-io]` selects this backend; Slint's core, API, and renderer
remain pinned upstream at 1.18.0. On a Slint upgrade, compare the upstream window
lifecycle handling and remove this patch once equivalent teardown is provided.

Nova's mpv notifier and recovery policy live in `crates/player/src/lib.rs` and
`crates/player/src/android_recovery.rs`. The policy tests run on desktop, but
native surface/context lifetime and decoder recovery need device validation:
repeat switching apps, lock/unlock, rotation, paused playback and initial loading
with HW+, HW and SW. Capture `adb logcat -s nova-player` and confirm teardown
release/completion precede setup on a different EGL context, with one recovery
submission per interrupted session/window cycle.

The ordering is required by [mpv's render API](https://github.com/mpv-player/mpv/blob/master/include/mpv/render.h):
all OpenGL render API calls, including freeing a context, require the same GL
context used at creation to be current.

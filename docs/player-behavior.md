# Player behavior

Closing the in-app player finalizes its observed progress and immediately rebuilds
Home’s Continue Watching and Upcoming cards, including the next released episode
and updated resume progress, without requiring a navigation change.

Local edits request device sync after 2 seconds without another edit, with a
10-second ceiling during continuous editing. Ordinary playback checkpoints use
a fixed 2-minute automatic push deadline that later checkpoints do not postpone;
the configured periodic sync remains a reliability fallback and may run sooner.
Pause, stop, episode replacement and completion save the latest observed resume
point and immediately request one pass for pending changes. Automatic requests
retain unavailable peers’ backoff; the manual Sync now action still forces a retry.
Repeated stationary paused ticks do not rewrite progress or request empty passes.

On Android, the activity Pause listener checkpoints on the UI thread before its
timer can stop, using the position observed while pausing mpv. This terminal
handoff can request a pass from the live app engine while backgrounded when sync
is enabled. It does not authorize continuous automatic background pushes or a
headless job lease; those retain their existing periodic scheduling constraints.

## Fullscreen stream selection

Movies show Start/Continue beside the library button and have no Episodes tab.
The action opens a fullscreen selector; choosing a series episode opens the same
selector with its episode context. Provider results arrive incrementally, and
request-specific downloads stay pinned before the remote results.
Titles whose addons cannot provide an episode list retain their direct stream
fallback through the same primary action.

The selector uses the phone layout below 700 logical pixels, a compact full-width
list from 700 to 1199 pixels, and a shaded artwork/context column from 1200 pixels
when the usable height is at least 600 pixels. Short landscape windows use the
compact layout with a single-row title/context header and visible stream feedback.
Resizing keeps the selected addon, page, stream focus and downloads.
All groups streams by the current installed-addon order as each cached or live
snapshot arrives; it displays available results while other addons are pending.
Local and synced addon reordering updates an already-open selector and its pills
immediately, retaining the chosen addon and focused stream by identity.
Nova remembers one most recently selected usable stream across synced devices for
seven days. When it is returned again for the same movie/episode, it is pinned
after downloads, tinted like a download and labelled **Last used**. Other rows
retain addon priority; filtering never brings back a hidden addon or invents a
missing stream. Matching uses the owner's stable provider ID or exact source
identity (torrent infohash/file index, or URL and request headers), ignoring
titles, descriptions, seed counts and changing subtitle URLs. URL-only addons
whose playback URL rotates may not match; no fuzzy title matching is attempted.
The `last_stream` sync domain holds one selection with only a fingerprint, version
and original selection time; resolved URLs and credentials stay local. Existing
LWW clocks choose the latest selection. Receiving it updates an open selector
without starting playback or renewing its seven-day lifetime. The local cache
expires on reads and its background worker tombstones expired authoritative
records at the original selection version without deleting newer local or offline
selections. Cache-file writes run off the UI thread.
Portrait and desktop keep addon filters in the header while results scroll
independently. Short touch landscape scrolls the context and cards together,
while addon filters pin at the safe top. An opaque full-width theme canvas
covers the pinned filters and the area above them, including safe-area insets.
Artwork starts at the top and fades into the canvas. True-black cards use a thin
gray outline for separation, with the stronger purple ring for keyboard focus.
Ordinary bottom subtitles rise above the bottom OSD with an 8-pixel logical gap
and follow its existing fade, including reduced-motion behavior. A higher user
position remains unchanged; hiding controls or closing playback restores the base
position. Native ASS dialogue already clear of the controls in letterboxed video
stays put, and mpv/libass retain authored positions and styling for signs/effects.
Nova changes only the transient `sub-pos` value and never forces ASS style overrides.
Stream rows offer playback and an actions button; desktop keeps native menus and touch
keeps hold-to-open sheets. Empty/loading states use the existing stream hints.

Back returns a manually chosen episode to its episode list and a movie to
Overview. Home Continue/Watch Now shortcuts return directly to their root page.
The selector has one Back control. Returning from the selector or closing playback
opened in Detail reveals the last watched episode, selecting its season/page,
clearing a filter that could hide it and scrolling its measured card into view.
Without viewing history, the selected episode remains the return anchor.
Movies and Home shortcuts retain their existing return routes. Keyboard
arrows navigate controls and streams, Enter plays the focused stream, Left/Right
or PageUp/PageDown change pages, and Escape/Back dismisses the nearest open layer.
Android system Back routes directly to the selector at the window capture layer;
it dismisses an open actions sheet first and ignores held-key repeats.

Implementation: `crates/ui/stream-selector.slint` and the AppWindow/Detail host;
`src/app/{detail,library,home,run}.rs` own entry/exit and callback wiring. Headless
coverage includes `tests/streams_layout/selector.rs` and the stream paging, downloads,
wrapped-row and addon-gesture tests.

## Player header, screenshots and keyboard controls

The OSD header shows `S1E1: Episode title` beside Exit, with the show's
name rendered as text below it. Movies show their title without an
episode line. The header fades with the controls; the bottom strip retains
playback controls and timing without repeating the title. The displayed title belongs to
the playback request, so browsing cannot replace it with another show's title.
Playback no longer fetches or carries title logos. Visible controls dim the entire playing video uniformly;
loading artwork uses the same dim layer, with loading/error text drawn above it. Brightness/seek/speed feedback stays at the
safe top center rather than shifting with header or screenshot-notice height.
Title captions fade out while feedback is visible and fade back in afterwards,
without moving or being unmounted. The existing player animation setting controls these fades.

The top-right screenshot button saves the video frame with subtitles and
without Slint controls. It is disabled while loading or capturing. Capture
and file writes use a worker, with a session guard and a 20-second capture
deadline. Android HW+ renders video and subtitles into a temporary GPU target
at the video’s display dimensions, then reads back RGBA pixels on the existing
render thread. Image encoding stays on the worker; the decoder, pause state and
playback position are unchanged. Other decoder modes use mpv’s screenshot
command. Desktop saves to Pictures/Nova (the configured Pictures folder on
Linux and Windows); Android API 29+ publishes to Gallery's Pictures/Nova,
while API 26–28 uses app-scoped Pictures/Nova. Temporary captures are removed
afterward. Settings → Player → Screenshots chooses PNG (lossless, the default), JPEG,
WebP lossless or WebP lossy. JPEG and lossy WebP have independent 1–100 quality
sliders (85 by default); lossless modes have no quality slider. These preferences
sync as independent general settings and support device overrides. Capture
composition and source display dimensions are unchanged. GPU readback encodes
the chosen format directly; mpv captures to a lossless PNG staging file before
worker conversion, so subtitle rendering remains unchanged.

The folder choice defaults to Pictures/Nova and can group shots under a show
subfolder. Names contain a shortened show title, full season/episode numbers
when available, the capture-request playhead position in milliseconds, and a
capture-time/counter suffix to avoid collisions. Season/episode padding is a
minimum (S100E1234 stays intact). Filename parts replace punctuation/whitespace
with underscores, avoid Windows device names and path separators, and limit the
show title to 40 UTF-8 bytes without splitting characters. A conservative
240-byte/UTF-16-unit destination bound rejects unusually long Pictures roots;
Android Gallery uses the matching MIME type, extension and relative path.

Screenshots are local files and never enter device sync. A brief
notice reports the saved location or a sanitized error.

Outside player menus, Left/Right seek by ten seconds, Up/Down change volume
(desktop five percentage points, Android one system-volume notch), Space/K
pause or resume, and desktop F toggles fullscreen. Held pause/fullscreen keys
toggle only once. Tab/ShiftTab move through OSD controls; Enter activates the
focused control. Open menus retain their arrow navigation. Closing playback,
including natural end-of-stream, leaves fullscreen before returning to the
catalog.

Implementation: `crates/ui/player.slint`, `src/app/{playback,posters,run}.rs`
and `crates/player/src/{screenshot,gpu_screenshot}.rs`. Header/keyboard regressions live in
`tests/player/{osd_header,shortcuts}.rs`.

Deferred addon streams open the built-in player immediately with “Resolving
stream…”, the selected show's header and the normal dimmed loading artwork.
Once the addon returns a URL, the status becomes “Loading stream…” and normal
playback/resume selection continues. Close or navigation cancels the request;
late results cannot reopen playback or replace a newer choice. Resolver errors
stay readable in the player, with Close returning to the selector. The existing
30-second resolver deadline is separate from the decoder's startup timeout,
which begins only after a media URL is queued. External-player preferences still
resolve before handing off; credential-bearing streams retain in-app playback.

## Buffered progress

The seek bar keeps its played portion purple and shows an estimated forward
buffer in a slightly lighter gray than the remaining track. The existing 250 ms
mpv tick reads `demuxer-cache-time`, falling back to position plus
`demuxer-cache-duration`; the result is clamped to the video duration. This is a
visual estimate, not a map of guaranteed cached seek ranges or torrent pieces.
Missing cache information and unknown duration hide the segment. Seek, source
replacement, resolver waiting, Android decoder reload and Close clear old buffer
state; the shared `buffered_until` property reaches the compiled OSD through
AppWindow/PlayerOsdBindings/PlayerOsdBridge. Both themes and phone/landscape/desktop
layouts use the same track.

## Android system gestures

System Back dismisses the nearest open layer without enabling keyboard focus
markers on the player or the page it returns to. Held repeats still pop only
one layer; desktop keyboard navigation keeps its focus markers.

Leaving the app with Home, switching apps, or turning the screen off pauses
playback immediately, including a stream still loading. Returning restores the
video and position and shows the OSD. Settings → Player → When returning to
playback offers Stay paused (default) or Auto continue. Auto continue resumes
only a video that was playing before leaving; manually paused videos stay
paused. Stay paused still opens/reloads the source at the saved position and
prefetches media through mpv’s normal cache; it does not wait for Play before
loading. Playback time and audio remain paused. Paused Android controls do not
auto-hide; press Play to continue.

The `CacheSettings.android_auto_continue` preference is device-local and never
synced; older settings default to false. Rotation alone does not change pause
state unless Android also pauses the activity.

Volume and brightness swipes ignore drags that begin within the top 48 logical
pixels, or within the top safe-area inset plus 24 pixels when larger. This
leaves Android notification-shade pulls alone even when Android delivers the
initial touch to the player before taking over. Interior swipes retain the
existing sensitivity and left/right brightness/volume mapping.

## Video taps and double-tap seeking

Double-tapping either outer third seeks backward/forward ten seconds and shows
the small seek readout. It preserves OSD visibility and pause state: hidden
controls stay hidden throughout both taps, and visible controls stay visible.
On desktop it does not trigger the single-tap play/pause action first.

A single tap on a seekable outer third waits for the 280 ms pairing window
before applying the single-tap action. Center taps act immediately. On Android,
single taps only toggle controls. On desktop, a primary single click always
pauses/resumes and shows controls, even if hover had already made them visible;
non-primary backdrop clicks do not trigger playback gestures. Space or K also
toggles playback, once per press, when a popup is not handling the keyboard.
Press-and-hold and swipe gestures cancel pending taps when recognized. Small
finger movement within the tap threshold does not reveal controls early.

A 500 ms hold temporarily previews 2× speed and shows only the speed readout,
without waking the OSD. Once recognized, the hold owns finger motion until release
or cancellation; moving across the video or controls cannot wake the OSD or start
a brightness/volume swipe. Releasing restores the effective title/default playback
speed and leaves OSD visibility unchanged.

Implementation and headless input coverage: `crates/ui/player.slint` and
`tests/player/{gestures,double_tap,hold_motion}.rs`.

## Playback defaults and title preferences

Settings → Player sets the built-in player's preferred audio and subtitle
languages and playback speed. Language controls use the existing Settings search
and dropdown styles, including sync badges and optional device overrides.
Automatic keeps mpv's normal track selection; subtitles also offer Off.

Choosing audio, subtitles or speed in the OSD saves that control for the series
(across episodes) or movie, even outside the library. Each submenu has
**Use global default**, which clears only that control and immediately reapplies
its current default. Speed previews from holding the video never save a choice.

Track preferences store language and track kind (normal/commentary audio or
full/forced/signs subtitles), not stream-local track numbers. A missing saved
track falls back to the global language; unavailable subtitles fall back to Off.
The saved choice remains available for a later stream that supplies it. Untagged
tracks can match only a unique title, rather than guessing a language.

Global defaults and independent title controls sync through the existing account
records. Remote changes take effect at the next playback opening, without
switching tracks or speed during an active session. Local default edits affect
only controls that still inherit. Streams, downloads, episode changes and
Android playback recovery use the same session preferences.

Implementation: `src/app/playback_preferences.rs` owns persistence and OSD
callbacks; `crates/config/src/playback_preferences.rs` matches semantic choices;
`crates/player/src/track_preferences.rs` reads mpv metadata. Regression coverage
includes their unit tests and `tests/settings_preferences/playback_preferences_ui.rs`.

Global audio and subtitle defaults in Settings → Player accept ordered language
lists. Add languages through search, then move or remove them with the row
controls. Playback chooses the first available language, preferring an exact
region match within that language. Existing per-title track choices still take
precedence; Automatic and subtitle Off clear the list. The existing synced
settings fields store comma-separated codes, so old single-language values and
device overrides remain valid. Supernova already exposes separate ordered audio
and subtitle language lists through the generic addon configuration editor.

## Next-episode banner

In-app episodic playback offers the next episode during the final two minutes,
capped at the final 10% for short episodes. The duration must be known and
playback must have started. Movies, unknown episode IDs, and episodes without
an available successor do not show an offer.

The successor follows the currently playing episode in season/episode order,
including season transitions. Specials (season 0) and entries without usable
regular episode numbering are excluded. Watched history does not change this
order. A known future release blocks the offer; a missing or invalid air date
uses the same availability rule as the episode picker.

The offer is a slim text-only strip in the bottom-right corner, capped at
360 logical pixels wide. It uses a translucent player-control surface, a
single-line episode number/title (elided when long), a quiet purple
**Choose streams** text action, and dismiss. Both actions retain at least
44-pixel touch targets, including in narrow layouts and translations; the
action label may wrap to two lines without stacking the strip. No episode
artwork is fetched or displayed for the offer. It remains available when the
OSD fades, sits above the bottom control lane while controls are visible,
and yields to open player menus.

**Choose streams** saves the current episode's progress, runs normal player
close cleanup, finalizes history, and opens the successor's existing stream
list. It selects by stable episode ID, clearing an active episode filter and
moving to the correct season/page. There is no autoplay: the user selects a
source through the normal stream flow, including existing resume preferences,
loading messages, empty results and errors. Choosing the action does not add a
new watched rule; the usual progress threshold still decides watched status.
Natural end-of-stream behavior is unchanged.

Dismiss lasts for the current stream opening. Seeking back out of the end
region hides an undismissed banner; returning to the region offers it again.
A replacement stream starts a new session, even for the same episode. Session
tokens reject old actions. Banner state is memory-only:
the banner adds no settings, storage keys, or sync records.

The banner does not steal keyboard focus when it appears. Tab navigation
reaches its source and dismiss actions. Back closes an open player menu first,
then dismisses the banner, then closes the player on a subsequent press.

Implementation: `src/app/next_episode.rs` owns eligibility and session guards;
`src/app/playback.rs` observes progress; `src/app/detail.rs` shares ID-based
episode stream selection with the existing picker; `src/app/run.rs` wires
callbacks and close cleanup. `crates/ui/player.slint` renders the banner through
the AppWindow `next_episode_*` properties/callbacks.

Regression coverage lives beside the policy in `src/app/next_episode.rs` and
in `tests/shared_layout/next_episode_banner.rs` for headless layout, navigation and input.

## Torrent request and file ownership

Each stream selection carries a playback request identity through resolution
and the resume prompt. Replacing it, backing out while it is pending, or closing
the player cancels that identity. Late success and error callbacks cannot open
a stale stream or update the current hint. Engine activation and cleanup share
an operation lock; cancellation is checked after discovery and initialization,
before committing ownership or evicting other cached torrents.

Offline torrent downloads have stable job owners independent of worker retries.
Their actual bundle directory and resolved file index are saved in the local
manifest and restored before cache sweeping. Multiple episode jobs can share a
single torrent handle and its selected files. Removing one owner preserves the
bundle for sibling downloads or active playback; torrent cleanup never deletes
an episode's parent directory through the generic HTTP artifact path.


## Player integration test pilot

`cargo test --test ui_player` links the six Player integration modules into one
executable. The existing assertions and mock/system-clock choices stay in
`tests/player/{double_tap,gestures,hold_motion,osd_header,osd_touch,shortcuts}.rs`.
Each case calls `tests/support/isolation.rs` before accessing Slint: the parent
reexecutes the same binary with an exact case filter, fresh data/cache/temp
roots and a receipt from the selected body. This preserves backend, locale and
storage isolation while keeping normal Cargo filtering and failure reporting.
The native loading-dimmer fixture remains ignored unless explicitly selected.
Discover and Library now use the same helper in their own suites; their
measurements are recorded in `docs/project-structure.md` under Testing.

A local default-Nix measurement on 2026-10-13 used two Cargo jobs and the same
symbol-stripped test profile for both layouts. Dependencies were already built;
case incremental caches were cleared for the executable-build comparison.
Single-case rebuilds changed the same assertion message and report the median
of three samples. All probe edits were restored. These are small local samples,
not whole-project clean-build measurements or general performance guarantees.

| Measurement | Six separate targets | One Player suite |
|---|---:|---:|
| Linked executables | 6 | 1 |
| Combined executable size | 995.19 MiB | 174.97 MiB |
| Build with cached dependencies | 12.286 s | 3.723 s |
| Single-case edit rebuild, median | 3.065 s | 3.164 s |

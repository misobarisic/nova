# Player behavior

Closing the in-app player finalizes its observed progress and immediately rebuilds
Home’s Continue Watching and Upcoming cards, including the next released episode
and updated resume progress, without requiring a navigation change.

Home also offers Watch history, newest actual playback first, with the most recent
episode of each title. It includes titles outside Library and partial rewatches
without clearing their watched status. Titles visibly shown in Continue Watching
are omitted from History. Continue Watching resumes an active partial rewatch
instead of jumping ahead. History cards open that exact episode's stream selector,
including when metadata must reload. Default layouts enable History after Continue
Watching; custom layouts can add/reorder it in Settings → Home.

Ordinary native playback retains the paused final frame and seekable timeline at
EOF. After five seconds at the end, a fullscreen menu offers Stay in player,
Next episode when available, and Show details over the dimmed episode loading
artwork. Staying dismisses this end screen; seeking backward cancels it and lets
you replay the ending with Play. Source replacement and close reset its delay. Backgrounding suppresses the
current end menu until playback leaves the end and reaches EOF again. Watch parties retain their coordinator-owned end behavior.

Settings → Player → Automatically play next episode is off by default and syncs
with device overrides. When enabled, actual EOF first looks up the immediate
successor through the ordinary verified stream cache/prefetch path. Movies,
unknown/future air dates, parties and remote-controlled playback never auto-advance.
All addon replies must finish within the ordinary five-second EOF delay; empty,
failed or slow discovery falls back to the existing end menu. Nova prefers the
same release family through its normal recommendation policy, then the first
playable stream in installed-addon/provider ranking (completed downloads remain
pinned). Completed downloads also participate in release recommendations, and
playing a saved episode remembers its show’s release preference. Downloaded
release text can match online sources without the original addon URL, while
online-to-online comparisons retain addon ownership. Incomplete or missing local
files never qualify. Completed download candidates must match the media type, parent title
and episode ID; an episode ID reused by another title is insufficient.
Unsupported streams and disabled torrents cannot be selected.
A known candidate opens a fullscreen artwork preview showing its episode, source
and a small eight-second countdown. Play now starts at the beginning through the
ordinary playback path; Cancel, Escape or system Back opens the existing end menu
immediately. Seeking away, replacing/closing playback, backgrounding, or changing
source settings/addons revokes pending work and the countdown. Late results and
repeated actions cannot start a second stream. No discovery activates resolvers,
torrents or playback before the preview is accepted or its countdown finishes.

Local edits request device sync after 2 seconds without another edit, with a
10-second ceiling during continuous editing. Ordinary playback checkpoints use
a fixed 2-minute automatic push deadline that later checkpoints do not postpone;
the configured periodic sync remains a reliability fallback and may run sooner.
Pause, stop and completion enqueue the latest observed resume point without
waiting for disk on the UI thread. The ordered persistence worker requests one
sync pass after that checkpoint commits; repeated requests share one bounded
watermark, and failed writes retain the handoff until retry succeeds. Episode
replacement, profile switching, Android backgrounding and orderly shutdown keep
their synchronous durability barriers. Automatic requests
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
The icon-only Lucide refresh pill sits before All and refreshes every source for
the current movie/episode. It retains the selected addon, resets results to page
one and bypasses ready/pending app caches. A two-second cooldown and in-flight
guard reject repeated activations; late responses cannot refill the fresh cache.
Each pending addon pill shows a quiet left-to-right sweep, or a static wash and
dot with motion disabled. The header omits a duplicate stream count and pager;
paging remains below the results.
Universal stream filters live only in Settings → Player, in the regular themed
settings card. Resolution, size, codec, language and unknown-metadata rules retain
profile sync and device overrides through the existing scope controls.

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
Subtitles retain their chosen position when controls or next-episode offers appear.
The OSD clearance measurements, fade logic and native subtitle lift/restore code
remain implemented, but `OSD_LIFT_ENABLED = false` disables lifting at runtime.
mpv/libass retain authored positions and styling for signs/effects.
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

## Playback rendering cadence

Slint owns window presentation. Normal mpv rendering uses
`MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME=0` together with
`video-timing-offset=0`, so a UI animation redraw does not wait for the next video
presentation deadline. Video redraws remain driven by mpv's update callback;
there is no permanent high-frequency timer.

Android clears the content frame-rate hint while controls are visible/fading,
playback is paused/loading or Slint has active animations. The render notifier
clears it immediately for an animated frame. Once only playing video remains,
Nova restores the container frame rate multiplied by playback speed, including
after surface recreation. It does not permanently request a particular 60/120 Hz
mode. `nova-player` unit tests cover these transitions; the opt-in
`measure_native_video_ui_composite_timing` diagnostic compares the old and current
render waits on a desktop display (`NOVA_PLAYER_TIMING_BASELINE=1` selects the old
path only in the test binary). Physical Android smoothness and panel-mode changes
still require device validation.

## Stream recovery and scene bookmarks

The automatic Try another stream prompt waits for 30 continuous foreground
seconds of startup/loading or stalled playback. Actual progress, a pause, EOF,
source replacement or a background gap resets the wait. The manual action in
player settings remains available immediately and retains semantic track preferences.
Settings → Player → **Automatically try another source** is off by default and
saved only on this device. When enabled, a stalled local playback run can try at
most two distinct alternatives, excluding its original source. Pause, background
suspension, navigation or cancellation stops that automatic run. Watch parties,
paired receivers and casting keep their existing coordinators. Automatic attempts
never overwrite Last used or the explicit per-show source preference; manual
source selection and recovery stay available after the automatic budget ends.

Nova separately remembers sources that produced 30 continuous seconds of real
foreground playback progress on this device. Seeks, pauses, buffering and long
tick gaps cannot establish success. The device cache holds at most 128 source
fingerprints for 30 days, without URLs, tokens or release descriptions, and never
syncs. Verified sources break internal recovery ties after the user's filters
and complete sort rules. The source picker keeps its existing presentation,
labels and ordering; no success indicator is shown.

A replacement source loads at zero, held paused before its first frame while its
own native duration is checked. Ordinary play and seek controls cannot release
that hold; only an accepted timeline or explicit resume/start-over decision can.
An exact source identity or identical torrent hash and explicit file index,
together with corresponding duration, can retain the previous absolute time.
Otherwise playback pauses and asks whether to resume at the previous time or
start over. Equal durations alone do not establish the same cut; Nova never
converts the old playhead to a percentage. Resume stays disabled when the old
position is outside the replacement's known runtime (or no usable runtime is
available); Start over and cancellation remain available. Cancel closes that replacement.
Paired receiver source changes use the same guard and receiver resume/start-over
controls; an unchanged source with matching runtime resumes directly without the
ordinary episode Ask prompt.

Native seeks require the current source to report seekability. An accepted seek
holds the requested timeline position for at most five seconds before returning
to the actual playhead if it does not land. Pause, EOF, decoder recovery and
startup-error checks continue during that brief hold, including while paused.

Playback → Scene bookmarks lists the current episode's saved scenes.
Each row keeps its note and timestamp on the left and Lucide edit, clear-note
and delete buttons on the right. More → Bookmarks groups saved scenes by title;
Play, Edit note and Delete use Lucide icons with visible labels in one row when they fit, with a
wrap fallback on narrow layouts. The Chapters settings row uses Lucide List video
artwork alongside the existing playback settings icons. The archive note editor
uses labeled checkmark Save, eraser Clear and X Cancel actions, with Save as the
primary button. These actions also wrap when needed. Opening a title and returning to
the title list slide/fade according to the existing page-transition settings;
the title list stays mounted to preserve its search, sort and scroll position.
Back closes a note editor before returning from the title.

## Subtitle menu

Playback → Subtitles and the desktop captions popup keep their existing track
lists and add two top shortcuts: Appearance & timing and Second subtitles.
These open the existing appearance/delay and second-language menus. Back returns
to the picker that opened the tool, retaining keyboard focus on its shortcut;
opening a tool does not change the selected main track.

Automatic addon-subtitle alignment maintains separate main and second-language
offsets. A full embedded dialogue track, an already-aligned addon in the other
visible slot, or sufficient retained embedded-reference evidence can anchor a
track. The main correction is not copied to the second track. If both visible
tracks are external and neither has a trustworthy timing anchor, Nova leaves
them unchanged. Matching runs on one bounded worker; the player applies only
results that still match the current source, seek, tracks and timing choices.
Manual edits win for their slot, and offsets remain local to the playing source.

## Player header, screenshots and keyboard controls

The OSD places the show's title, optional episode caption, live source badges and
available year/genres at the bottom left. Android landscape keeps circular
play/pause and ten-second seek controls over the center of the video, with compact
subtitle, audio, speed, screenshot and settings buttons beside the
metadata. Desktop keeps playback and volume at the bottom left and utilities on
the right, beneath the timeline. Portrait stacks utilities below the timeline;
track selection remains available through Settings at narrow widths. Time elapsed
and total duration sit at opposite ends of the safe-area-aligned seek bar.
Dragging the timeline previews the target position and timestamp locally, then
seeks once on release. Native playback continues during the preview; cancellation
discards it. A hidden timeline only wakes the controls on its first press, and
loading/live streams or party members without control permission cannot scrub. Held previews keep controls awake.
Android enters immersive playback automatically and has no fullscreen button;
desktop retains its fullscreen control.
Badges use mpv's actual resolution, video codec, HDR transfer and selected audio
language/channel layout, omit unavailable facts, and reset on source replacement.
Metadata belongs to the captured playback request, so browsing another title does
not replace it. The party badge remains in a separate safe top lane. Subtitle,
intro-skip and next-episode clearance measures the entire footer.

The full viewport dims lightly while controls are visible, with a stronger bottom
gradient behind the footer. Loading artwork retains the stronger uniform dimmer;
loading/error text paints above it. Captions fade during gesture feedback without
changing geometry. Feedback and screenshot notices retain stable safe top lanes.
All controls share the existing player motion setting and hidden-control wake guard.

The screenshot utility button saves the video frame with subtitles and
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

In Standard interface mode, outside player menus, Left/Right seek by ten seconds, Up/Down change volume
(desktop five percentage points, Android one system-volume notch), Space/K
pause or resume, desktop F toggles fullscreen, and S saves a screenshot.
Screenshot keys are disabled while loading or capturing; modifier chords and
open menus retain their existing ownership. Direct playback shortcuts preserve
a hidden OSD; Tab/Enter and menu navigation reveal controls and focus markers.
Screenshot confirmation appears independently without opening the controls.
Held pause/fullscreen/screenshot
keys act only once. Escape dismisses the nearest player menu or next-episode
offer first, then closes the stream, including fullscreen playback.
Tab/ShiftTab move through OSD controls; Enter activates the
focused control. Open menus retain their arrow navigation. Closing playback,
including natural end-of-stream, leaves fullscreen before returning to the
catalog. Desktop remembers the last playback fullscreen choice locally and
restores it when the next stream starts (including addon resolution), without
fullscreening the catalog. The desktop window starts maximized with no saved
geometry; later launches restore its normal logical size and maximized state.
Playing controls hide after three seconds of inactivity on both platforms;
startup publishes the live pause state before arming that countdown, avoiding
loading’s five-second paused timeout. Desktop hides the pointer once controls
finish fading out and restores it on activity or player exit; loading, menus
and the next-episode offer keep it visible.

Implementation: `crates/ui/player.slint`, `src/app/{playback,posters,run}.rs`
and `crates/player/src/{screenshot,gpu_screenshot}.rs`. Header/keyboard regressions live in
`tests/player/{osd_header,shortcuts}.rs`.

The native screenshot regression uses single-pass rendering when explicitly run
with `LIBGL_ALWAYS_SOFTWARE=1`: the installed legacy multipass renderer can return
black frames under headless llvmpipe, including in isolated runs. Its assertions
still check actual GL rendering/readback, orientation, subtitles, encoding and
host-state restoration. These CPU-fixture runs do not validate multipass rendering;
hardware/default test runs and production initialization retain that path.

TV interface mode uses two remote-focus rows. With controls hidden, OK toggles
pause/play and reveals the OSD together; Left/Right previews a ten-second seek
without opening the full overlay. Holding a direction accelerates the preview;
release performs one bounded seek. Up/Down can reveal controls without pausing.
With controls visible, Left/Right moves between bottom buttons; Up focuses the
timeline and Down returns to the originating button. Left/Right on the timeline
previews a seek and commits on release. Down from the button row or Up from the
timeline hides controls. Back closes a menu, cancels a pending seek, hides the OSD,
then leaves playback. Loading/live streams and party members without control permission cannot seek; previews
are discarded if focus, control permission or the current source disappears. A held
seek keeps visible controls awake. Android's mobile center transport is replaced
by the TV bottom row, which also contains screenshots; TV volume uses the remote's
system control. Mouse clicks, wheel menus and pointer timeline scrubbing remain
available on desktop; clicking the video toggles pause and reveals controls.
Android's Play/Pause toggle, separate Play and Pause, Stop and seek buttons use the same actions;
Next opens the available next-episode offer, while Previous is consumed without
inventing a previous-episode target. TV-specific regressions, including mouse
switching and party guards, live in `tests/player/tv_controls.rs`.

Deferred addon streams open the built-in player immediately with “Resolving
stream…”, the selected show's header and the normal dimmed loading artwork.
Once the addon returns a URL, the status becomes “Loading stream…” and normal
playback/resume selection continues. Close or navigation cancels the request;
late results cannot reopen playback or replace a newer choice. Resolver errors
stay readable in the player, with Close returning to the selector. The existing
30-second resolver deadline is separate from the decoder's startup timeout,
which begins only after a media URL is queued. External-player preferences still
resolve before handing off; credential-bearing streams retain in-app playback.

## Linux system media controls

Play/pause publishes the native pause state to the controls immediately and
requests a redraw. Linux screen-saver/logind inhibition runs on one worker,
including release on pause/EOF/close; its bounded mailbox coalesces rapid
transitions to the latest desired state. Slow or unavailable D-Bus services
therefore do not block the controls or queue a service call for every click.
A pause/resume boundary permits another acquisition attempt after failure,
even when both requests arrive during an earlier blocking call.

Linux desktop builds expose the built-in player's current title and episode,
duration, position, playback state, volume and speed through MPRIS on the session
bus. Desktop media widgets and tools such as `playerctl --player=nova play-pause`
can pause/resume, stop, seek, adjust volume/speed and raise Nova's window. Each
running process owns `org.mpris.MediaPlayer2.nova.instance<PID>` so multiple
instances do not replace each other's service.

Commands use the same UI-thread dispatcher as the player controls. Resume
confirmation, watch-party control permissions and local/remote playback ownership
still apply; queued commands are discarded if their source, profile, room or
control-policy revision has changed. Everyone rooms allow authorized guests as
well as the creator; party pause feedback follows the pending/logical state and
speed reports the accepted room target, without changing personal preferences. Next and
Previous are unavailable because Nova does not expose an MPRIS playlist. Opening
arbitrary URIs and quitting the application are unavailable. Stopping closes the
local player through the usual progress-save path. Playback on a paired receiver
is not exposed as a local media session.

Metadata contains display titles and an opaque per-source track path, without
stream URLs, authorization headers or local filenames. No setting or stored data
is added. D-Bus registration and signals run on a worker, with coalesced state
updates from the existing 250 ms UI timer; ordinary position progression does not
emit property changes, while successful seeks and nonzero playback starts emit
`Seeked`. The reported speed follows title overrides and temporary speed holds.
A missing session bus disables this integration without interrupting playback.

## Windows system media controls

Windows desktop builds expose the same local title/episode, playback state,
position, duration and accepted speed through System Media Transport Controls
(SMTC). Media keys and Windows media widgets can play, pause, stop, seek and
request a supported speed. Next/Previous, channel changes and arbitrary URI
opening are unavailable. Volume continues through Windows' normal volume controls.
Profile locks, cast/receiver ownership, source replacement, resume confirmation
and party permissions use the same guarded dispatcher as Linux and Nova's OSD.
Stopping a guest's source retains the room's accepted episode.

The integration lazily attaches to Nova's top-level HWND after Winit creates it,
using desktop `ISystemMediaTransportControlsInterop::GetForWindow`. A dedicated
worker owns the COM apartment, event registrations, metadata/timeline updates
and teardown. One bounded wake carries the newest snapshot, so native services
cannot accumulate UI work. Timeline positions convert microseconds to Windows'
100 ns ticks; normal playback refreshes every five seconds, with immediate
updates on pause, seek, rate, source or duration changes. Closing, casting or
locking the profile clears the metadata and disables the session. Worker cleanup
unregisters handlers before releasing the apartment. No artwork is fetched and
no stream URL, headers, credentials or local filenames are exported. Windows
API failures disable the integration without interrupting playback.
Windows and Android do not register the service.

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

Home or switching apps while in-app video is playing enters Android picture in
picture on supported devices (Android 12+ automatic entry, Android 8–11 Home
callback). PiP contains the native video and system play/pause actions; Nova's
controls, menus and modal layers stay hidden until returning. Its aspect ratio
follows the selected video's display dimensions within Android's supported
range. Paused, unloaded, failed, audio-only and external playback do not
auto-enter PiP. Screen lock, PiP dismissal and activity Stop pause and checkpoint
playback. If Android retains the pinned window across lock/unlock, becoming visible
again restores its native recovery and Play action without requiring fullscreen
expansion or an Activity Resume. Stay paused/Auto continue and manual pause
choices still apply; watch parties still wait for the host timeline.
Disabled/unavailable PiP follows ordinary background pausing.

Settings → Player → Picture in picture enables this behavior by default on
Android. The choice is saved only on this device, applies to every title, and
is hidden on desktop. Turning it off immediately prevents further automatic
entry. An existing PiP window is paused and remains video-only until expanded
or dismissed; Nova does not force an activity launch to leave PiP.

Leaving the app without visible PiP, or turning the screen off, pauses
playback immediately, including a stream still loading. Returning restores the
video and position and shows the OSD. Settings → Player → When returning to
playback offers Stay paused (default) or Auto continue. Auto continue resumes
only a video that was playing before leaving; manually paused videos stay
paused. Stay paused still opens/reloads the source at the saved position and
prefetches media through mpv’s normal cache; it does not wait for Play before
loading. Playback time and audio remain paused. By default, paused Android controls stay visible; press Play to continue.
The paused-controls preference below can restore their ordinary idle hide delay.

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

## Casting remux buffer

Settings → Player → **Casting remux buffer** selects a 32, 64, 128 or 256 MiB
budget for retained media segments when casting needs packet-copy remuxing.
The default is 32 MiB on Android and 64 MiB on desktop, including older settings
without this field. Processing buffers and other app memory are additional;
this setting does not cap total app RAM. A larger budget retains more media
without changing codec support or enabling transcoding.

The choice persists and syncs through the ordinary settings record, supports a
device override, and applies to the next casting session. It is available for
both local-player backend choices because casting can start from a title's
stream picker independently of local playback. `CacheSettings.casting_remux_buffer`
uses the validated `nova_config::CastingRemuxBuffer` enum; `.bytes()` supplies
the captured presentation budget. Settings coverage is in
`tests/settings_preferences/casting_remux_buffer.rs`.

## Controls while paused

Settings → Player → **Keep controls visible while paused** controls whether the
built-in player's OSD stays visible while paused. The default preserves existing
behavior: on for Android, off for desktop. Older settings without the field use
that platform default. Explicit choices persist and sync through the existing
settings records, with the usual optional device override.

When off, paused controls fade after five seconds of inactivity; playing controls
still fade after three seconds with either choice. Pointer, touch and navigation
input reveal controls and restart the delay. Open player menus, loading/error
screens and active scrubbing retain controls; closing a menu or ending a scrub
starts a fresh delay. Idle keyboard/TV focus alone does not pin the controls.
Explicit Back/tap dismissal and playback shortcuts that keep a hidden OSD hidden
still work with the preference on. Changing the preference during a pause updates
the timer immediately without waking explicitly hidden controls.

The timer lives in `crates/ui/appwindow.slint`; `src/app/run.rs` handles activity
notifications and native cursor/frame-rate updates. `PlayerOverlay` publishes
its transient menu/scrub hold through `PlayerOsdBridge.osd_auto_hide_blocked`.
Headless regressions live in `tests/player/paused_controls.rs` and
`tests/settings_preferences/paused_controls.rs`.

## Playback defaults and title preferences

Settings → Player sets the built-in player's preferred audio and subtitle
languages and playback speed. Language controls use the existing Settings search
and dropdown styles, including sync badges and optional device overrides.
Automatic keeps mpv's normal track selection; subtitles also offer Off.

Choosing audio, subtitles or speed in the OSD saves that control for the series
(across episodes) or movie, even outside the library. Each submenu has
**Use global default**, which clears only that control and immediately reapplies
its current default. Speed previews from holding the video never save a choice.
The gear panel slides and fades into submenus and reverses direction on Back.
Its height animates to each page’s content, capped by the safe viewport with
scrolling for larger pages. Disabling player animations or all motion changes
pages and height immediately.

Subtitle appearance and timing has the same global/title split. Settings →
Player sets timing offset, text size, side margins, vertical position, outline
and text color. The in-player settings menu saves only the fields edited for
that title; **Use global defaults** clears its appearance/timing overrides while
retaining language and speed choices. A positive offset shows subtitles later.
Size, margins and outline scale from a 720p canvas. Embedded ASS styling remains
preserved by default, with an explicit option to override it; image subtitles
retain their source appearance. Untouched text size uses Nova’s 56-unit 720p-canvas
default, including older settings with no explicit size; previously saved sizes
are preserved. This is a relative mpv size, not Android sp. Untouched outline uses
the native mpv default; only explicit edits persist and sync. Position edits remain
the baseline independently of OSD visibility.

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
includes their unit tests and `tests/settings_preferences/{playback_preferences_ui,subtitle_style}.rs`.

Global audio and subtitle defaults in Settings → Player accept ordered language
lists. Add languages through search, then move or remove them with the row
controls. Playback chooses the first available language, preferring an exact
region match within that language. Existing per-title track choices still take
precedence; Automatic and subtitle Off clear the list. The existing synced
settings fields store comma-separated codes, so old single-language values and
device overrides remain valid. Supernova already exposes separate ordered audio
and subtitle language lists through the generic addon configuration editor.

## Intro and recap skipping

When trustworthy timing data matches the current episode and cut, the in-app
player offers **Skip intro** or **Skip recap** during that interval. The button
stays above controls, reserves subtitle clearance even with the OSD hidden, and
uses the existing offer navigation lane. It seeks the existing player; it never
reopens a stream or selects a next episode.

**Settings → Player → Intro skipping** offers **Off**, **Show skip prompt**
(the default), and **Auto skip**. The global `intro_skip_mode` setting syncs
independently of next-episode behavior. Off hides the button and avoids playback
lookups. Playback → Intro skipping applies an explicit Off/Prompt/Auto
choice to the current show; **Use global default** clears only that field. These
choices use control 4 in the existing typed-title `playback_preferences:v1` store
and sync domain, independently of audio, subtitle, appearance and speed choices.
The running player caches the typed override; each tick combines it with the
current global setting without reading the preference database.

Auto skip seeks past each intro/recap at most once in the current native/logical
playback session. Explicit seeks and replays retain manual control: going back
never immediately skips the segment again. Automatic action requires foreground,
started, unpaused, seekable local playback outside watch parties and remote-control
surfaces. Credits and mixed-story openings are never skipped.

Timing lookup uses the public [IntroDB v4 API](https://theintrodb.github.io/theintrodb-npm/getting-started.html)
for explicit IMDb or TV-typed TMDB identities and captured season/episode
coordinates, or [AniSkip v2](https://api.aniskip.com/api-docs) for a confirmed MAL
anime ID with season-one episode numbering. Other anime seasons need confirmed
release-specific numbering and are left unsupported. It uses current source
evidence, excluding historical identity hints; it never searches for identities.
One bounded playback request runs at a time (eight seconds, 128 KiB), without
keys or accounts. Streaming timings remain session-local. Missing data, errors, ambiguous IDs,
overlapping segments, duration differences over five seconds, and IntroDB
confidence below 0.8 produce no action. Only finite intro/recap intervals ending
within the first 60% of the episode and leaving content afterwards are accepted.
Native and logical session guards discard old source responses.

Episode downloads capture verified timing-provider coordinates in `downloads:v1`.
After the video transfer, an isolated mpv core reads local container duration
before decoder creation (`crates/player/src/media_probe.rs`); local HLS is
supported and nested network access is disabled. A probe that cannot determine
duration safely leaves timings unavailable. Preparation stays inside the existing
download worker so Android's foreground download service remains active. Timing
transport failures get up to three attempts with one- and three-second backoff;
each request is bounded to eight seconds and 128 KiB. Pause/removal cancels retries.
Missing timings or failed preparation never fail the video download.

Downloads prepare timings independently of the selected skip mode, so Off can
be changed later while offline. Only validated intro/recap intervals are saved
alongside their duration and artifact size/modification fingerprint in the job;
there are no sidecars or separate timing-cache keys. Offline playback checks the
fingerprint and actual native duration again and can restore numbering even after
the episode cache is evicted. A later online playback can fill missing timings.
Removing a download clears its timing data with the job; late workers cannot
restore removed jobs or attach data to a reused job ID or changed artifact.

Implementation and policy tests: `src/app/intro_skip.rs`; headless overlay
regression: `tests/player/intro_skip.rs` in `ui_player`.

## Next-episode controls

The footer's Lucide SkipForward button starts the right-hand control group,
before Subtitles, and opens the next episode's stream selector at any point
after episodic playback starts. It appears when the loaded episode
inventory contains a released, sequential successor with a usable season. Movies,
final episodes, future releases, loading and remote playback do not offer it.
The button saves normal progress and closes the current source through the same
cleanup as **Choose streams** below; it never chooses or starts a source itself.
In watch parties it uses the shared host/everyone control permission and waits
for the creator to accept the episode change before opening the local picker.
Dismissal of the timed banner does not remove the footer button. Source and
profile tokens reject actions left over from another playback session.

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
callbacks and close cleanup. `crates/ui/player.slint` renders both controls through
the AppWindow `next_episode_*` properties/callbacks. The footer uses separate
`next_episode_button_available` and `next_episode_button_token` properties, sharing
the existing `next_episode_streams` callback without changing party wire actions.

Regression coverage lives beside the policy in `src/app/next_episode.rs` and
in `tests/shared_layout/next_episode_banner.rs` for banner layout and input.
`tests/player/next_episode_control.rs` covers footer navigation, permissions and
compact layouts; `src/app/party_tests.rs` covers accepted party transitions.

## Synced watch queue and continuous playback

The Watch queue is personal profile data. Movies and exact typed episode
identities are separate membership records; order and the continuation cursor
are independent last-writer-wins registers. A losing reorder cannot delete
another device's addition, and removed rows cannot return through an old order.
The v1 queue migrates once to `watch_queue:v2` together with its sync records and
baselines; its original bytes remain available. Concurrent additions can exceed
the ordinary 200-item admission limit without being discarded.

**Play** still opens one item's ordinary source picker. **Continue queue** and
**Start from this item** create explicit receiver-local runs. Continue skips
watched rows, except an unfinished intentional rewatch, and never wraps a
finished cursor to the beginning. Completed rows stay until Remove or Clear
watched. A source picker alone does not consume an item. Accepted native starts,
explicit Skip and true EOF update the shared cursor; playback position remains
in ordinary progress. Receiving a cursor or autoplay preference never starts or
retargets a player.

Autoplay is off by default. A run captures the choice made at its explicit start.
At EOF its next surviving row takes precedence over sequential-series autoplay.
The existing bounded discovery, release-family preference, playable ranking and
eight-second source preview are shared. Matching completed downloads participate
only while the file exists and its full typed identity and profile policy agree.
Different titles use their own saved preference. Future or dateless episodes,
missing sources, timeouts and failed sources pause on the pending item and open
the normal source picker; Retry, Skip and Stop remain available. With autoplay
off, the EOF menu offers **Next queued item** and opens that planned item's source
picker rather than substituting a sequential episode. Cancelling the
preview pauses the run. Manual Skip does not mark the item watched. Reordering
affects the next transition; removing the playing row preserves its successor
context rather than abruptly changing the active media.

For paired Nova playback, Continue queue on the remote controls sends a
session-bound request containing the selected profile and optional item key.
The receiver requires the same selected, unlocked profile and its own synced
item, then resolves and plays its own source. It owns every advance and exposes
run/countdown state plus Skip, Stop queue and Play now. Controller disconnection
does not stop the run. Stale sessions and missing items fail without silently
switching profiles. External players and watch parties cannot run the queue.

## Sleep timer and transition precedence

Player and paired controls offer duration presets, a custom 1–240-minute
duration, End of this item and After 1–10 episodes during series playback.
Duration uses elapsed monotonic time, including pause, buffering and suspension;
the native player watchdog also enforces its deadline. The receiver owns a
paired timer and publishes sampled remaining time. It continues if the phone
disconnects. Timer state is local to the playback session and never restored or
synced as personal data.

End of item binds to the exact typed item. Episode counts include the current
episode and count each natural native EOF once; seeks and manual skips do not
decrement. Authorized queue or series continuation preserves the count, while
unrelated playback cancels it. An episode timer stops before a movie in a mixed
queue. Explicit Stop, player close or profile cancellation clears the timer.
Parties offer duration only and expiry leaves the party and stops this device;
external-player handoffs expose the timer as unavailable.

The single EOF path checks explicit cancellation and sleep expiry before party
ownership, then an active queue run, sequential-series autoplay and the normal
end menu. Final asynchronous starts recheck the timer, profile activation,
native load, source rules and run/item identity. A late lookup or countdown
cannot reopen playback after Stop or expiry.

## Torrent request and file ownership

Each stream selection carries a playback request identity through resolution
and the resume prompt. Replacing it, backing out while it is pending, or closing
the player cancels that identity. Late success and error callbacks cannot open
a stale stream or update the current hint. Engine activation and cleanup share
an operation lock; cancellation is checked after discovery and initialization,
before committing ownership or evicting other cached torrents.

Settings → P2P → No caching uses an isolated RAM-only torrent session. It never
writes torrent payload, metainfo, resume state or DHT state to disk, and does not
spill to disk when memory fills. It also never adopts a same-hash disk session.
Metadata discovery reuses the process-lived session's nonpersistent warm DHT,
then passes validated metainfo and discovered peers to an isolated RAM payload
session without reusing disk handles or payload.
The storage mode applies to new streams: already-playing streams keep their
chosen storage until replaced. A pending resolution rechecks the current mode
before activation, so enabling No caching cannot activate an older disk-backed
resolution. Turning it off selects ordinary disk caching for new streams.
Explicit Download jobs always use the separate guarded disk path, including
when No caching is enabled. RAM budget and download-rate changes apply live.
RAM-only torrents use the embedded player; external handoff is disabled so
Nova retains control of packet caching and prevents disk spill.

Torrent RAM buffer is a device-local buffering budget in MiB: desktop defaults
to 256 (32–2048), Android to 64 (32–512). Older saved settings get the platform
default; malformed values fall back safely and extreme values are clamped.
This budget covers torrent pieces and their in-flight reservations, including
reserved transport headroom; mpv's separate compressed packet buffer is
additional. RAM mode preserves mpv's normal packet limits and disables its disk
cache. Reducing those packet queues can prematurely end interleaved audio/video
tracks, so the torrent budget does not shrink them. It is not a cap on total app
RSS, decoded frames, or GPU memory.
A lower budget immediately stops new allocations above the new limit; existing
readers release their reservations before usage falls to it. The player status
reports actual RAM usage rather than disk cache size. Disk usage and Clear
continue to describe existing disk data.

RAM playback verifies pieces, prioritizes pending HTTP Range reads, evicts
unneeded pieces and refetches them when seeking. Startup and refill targets
adapt within the budget: healthy torrents start sooner, while slow or unstable
torrents build a larger reserve. Playback cannot overcome a sustained download
rate below the media bitrate. Memory pressure waits or fails visibly instead of
falling back to disk. RAM sessions do not upload or advertise HAVE: BitTorrent
v1 cannot retract advertised pieces after eviction. Oversized torrent pieces
may require a larger buffer budget to keep concurrent reads bounded.

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

## Windows and Linux decoder preferences

Settings → Player → Video decoder offers HW+ (try direct hardware decoding),
HW (copy-back hardware decoding), and SW (software decoding).
Windows defaults to HW; Linux defaults to HW+.
The preference is device-local. HW+ retains copy-back/software fallbacks when
the graphics context or driver cannot provide direct decoding.

The player's Hardware decoding submenu applies only to the current stream.
The next stream starts from the saved Settings preference. The selected row
tracks mpv's actual decoder, so a fallback is shown as HW or SW rather than
continuing to claim HW+. Windows and Linux change the decoder within the
existing source through mpv; Android retains its existing reload/fallback behavior.

## Device listening effects

Settings → Player → Night listening and dialogue and the same in-player
settings submenu share three saved, default-off device preferences:
`dialogue_boost` (a 4 dB speech-range EQ lift), `loudness_normalization`
(real-time dynamic leveling) and `night_mode` (dynamic range compression).
These fields stay in local settings JSON and are excluded from settings sync;
they do not alter per-title audio-track preferences or the wire protocol.
External players and casting receivers use their own processing.

Dialogue boost changes the whole mix; it is not voice isolation. Leveling uses
FFmpeg `dynaudnorm` in real time, rather than an offline measurement of a whole
program's integrated LUFS. Night mode uses `acompressor`. All enabled
combinations begin with 6 dB headroom and finish with `alimiter` (0.89 ceiling,
automatic makeup disabled). The DSP can introduce audio buffering; mpv owns
A/V synchronization. Nova does not change the user's volume or audio track.

`crates/player/src/audio_processing.rs` owns only `@nova-listening` in mpv's
`af` list. Edits replace that label immediately on decoded audio, without
reloading the source; turning everything off removes it. Other filters remain.
New sources and renderer reloads re-arm validation, and audio-format changes
recheck the graph. Idle mpv accepts even unavailable FFmpeg filters, so Nova
waits for real `audio-params` before installing and claiming success. Native
errors or a disabled/missing labeled filter remove the effect and show an
unavailable status in both menus. A failed attempt is retried only after a
preference, source or audio-format change.

The Android/Windows native media bundles pinned at vendor v0.1.2 enable
`equalizer`, `dynaudnorm`, `acompressor` and `alimiter`; their bundle inspector
rejects missing listening filters. The native smoke probe configures all seven
combinations for mono, stereo and 5.1. All four release targets passed their
build and bundle checks. Linux uses the installed mpv/FFmpeg filters; runtime
validation remains required on every platform. Cross-build checks do not
establish physical-device audio quality or latency.

## Source-provided caption metadata

Captions attached to a stream retain their declared language through resolution,
resume prompts, native playback and same-session Android recovery. A bounded
three-worker pool fetches them independently of video startup, with a twenty-second
request timeout, five-redirect limit and eight-MiB file limit. Captions on the
video's exact origin can use its request headers. Captions on other origins get
none of those headers; a redirect leaving the origin permanently drops them for
that request, including custom token headers and redirects back to the video
origin. Video authorization stays on the native video connection.

mpv receives only local caption files, so its global video headers cannot leak
through subtitle requests or redirects. Files belong to the accepted playback
session and are removed on replacement, close or rejection of a late result.
Android recovery reloads retained local files without another remote fetch.
The native Linux security probe also supplied cached M3U/HLS, DASH and ffconcat
reference bodies through both subtitle loading paths. No nested network request
occurred: FFmpeg rejected HLS detection for the fixed `.sub` cache extension,
mpv rejected playlist-only external subtitles, DASH's nested HTTP open was
blocked by the local-file `file,crypto,data` protocol whitelist, and ffconcat
rejected unsafe remote names. These observations cover the installed native
stack; physical Android execution remains unverified.

`SourceSubtitle` and the accepted source's caption inventory are runtime-only.
The shared native inventory reader matches an external subtitle's complete
`external-filename` to its local file and exact supplied URL, including its query,
and checks both the video source and playback session. It fills an unknown language from
the declaration and marks the caption as Included with stream in track menus.
An already known native language takes precedence. Unknown or conflicting
language declarations remain unknown; filenames are never language evidence.

Primary and secondary semantic selection, automatic addon fallback, retries
and menus use the same enriched inventory. A provided English caption can
therefore satisfy English without silently being replaced by another subtitle
addon. A higher-priority language that is genuinely absent can still use an
addon, and explicit viewer choices and Off remain authoritative. A new source
or Close clears the inventory; renderer/decoder recovery of that session keeps
it without persisting signed URLs, request headers or provenance in preferences.

This corrects a demonstrated metadata-loss path. It does not establish the cause
of any particular caption timing report or apply a numeric subtitle delay.


## Watch-party control and episode permissions

Watch parties default to host-only playback and episode selection. The creator
can enable “Allow everyone to control playback and choose episodes” before
creating the room or change it live. Everyone permits all members to pause,
resume, seek and choose the first or any other episode, season or title. The
creator still serializes the shared timeline and media generations; invites,
removal, wait-for-everyone, continue-without-waiting and ending the room remain
creator-only. Guests see the accepted policy as read-only status.

Authorized pause/resume clicks update the room and player transport icons as soon
as the local queue accepts them. Rapid toggles use the latest pending intent,
not an older acknowledgement. Adjacent pause requests coalesce without crossing
seek, rate or media actions; a stale reply cannot clear a newer request. The
projection is bound to the room, media generation, permissions and local source,
and rolls back on rejection or invalidation. Native playback still follows the
authoritative scheduled start and buffering holds, separately from icon feedback.

In both modes every member chooses their own stream for the current accepted
episode. Host-only guests can browse metadata but cannot open playback pickers
for other episodes; guidance offers the current episode’s source picker or
leaving the room. An empty host-only lobby waits for the creator’s first choice.
Live permission changes cancel disallowed seek previews and restore TV focus to
available current-episode actions. These temporary room permissions are not
persisted or synced; the policy and its revision use `nova/party/6`.


Manual Next episode in a watch party uses the same accepted control policy as
pause and episode selection. It proposes the released successor before closing
the current local source; acceptance opens this device’s source picker for that
episode. Rejected or stale proposals keep current playback intact unless another
member’s newer accepted selection is followed. The Next banner, keyboard/TV action and native media key are disabled while waiting for
acceptance or when the member lacks permission. Dismissing the offer stays local.


The same room policy governs shared playback speed. The target starts at 1×,
accepts 0.5–2× in 0.05 increments, and can be changed or reset to 1× in the lobby
and during playback. It survives episode changes and returns to the lobby.
Every member sees the accepted target in the room and player speed menus;
host-only guests can open the submenu as a read-only view. Requests wait for an
accepted room snapshot before changing the readout, and never save title/global
speed preferences. Leaving restores ordinary local playback preferences.

Drift correction runs around that shared target. For example, a room at 1.50×
may briefly use a native 1.545× rate while catching up, while all room speed
labels continue to show 1.50×. Native party rates allow 0.485–2.06× transient
headroom around the target bounds. RAM buffering and torrent consumption budgets
use this actual native rate, so faster shared playback reserves the appropriate
wall-clock buffer without persisting a speed override.

## Stats for nerds

The player's Settings menu opens a read-only diagnostics overlay independently
of the ordinary OSD. Everyone watching a party can view it, including guests
without playback-control permission. It samples native facts and the active
torrent about once per second while visible. Closing the panel stops sampling;
closing/replacing the source, switching profiles, locking a profile or changing
to cast/receiver playback clears it. A delayed sample or copy result cannot enter
a replacement source or a reopened panel. One in-flight sampler remains bounded
across those transitions and Android activity replacement.

Drag the panel by its header with a mouse or touch. Its position stays within
the player's safe area when the window resizes or rotates, and moving it does
not resize the card or change playback. Closing and reopening statistics keeps
the position during the same player session; closing playback resets it.
Mouse movement over the header, body or buttons restores the normal cursor and
controls and restarts their idle timer.

Dimensions, content FPS, actual display refresh, codecs/decoder, measured bitrate,
drop counters, audio, A/V sync, renderer and actual playback speed appear only
when available. Actual speed retains three decimals, so a party's temporary
1.545× correction is visible separately from its accepted shared target. Drop
counts have no invented presented-frame denominator; unavailable facts show an
em dash rather than guessed values. The physical render-window size may supply
the viewport when native viewport dimensions are unavailable.

The demuxer's packet cache is separate from the torrent's RAM piece pool.
Torrent download rate converts the engine's bytes per second to byte-rate units;
peer count, selected-file size, RAM usage/limit and buffer target come from the
active engine. RAM-only is shown only when an actual nonzero RAM limit is known.
Ordinary network and local media use generic source labels.

Copy diagnostics copies the same technical whitelist plus Nova's version and
platform. It excludes display titles, filenames, paths, URLs, account/profile
names, info hashes, headers, credentials and peer addresses. Clipboard helpers
run off the UI thread; Windows window ownership is captured before that handoff,
and Android uses its thread-safe JNI clipboard path. A success notice appears
only after the clipboard write succeeds. Stats adds no persisted or synced data.

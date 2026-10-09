# Player behavior

Closing the in-app player finalizes its observed progress and immediately rebuilds
Home’s Continue Watching and Upcoming cards, including the next released episode
and updated resume progress, without requiring a navigation change.

## Android system gestures

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
before toggling the OSD. Center taps act immediately. On Android, single taps
only toggle controls; on desktop, waking hidden controls also toggles playback.
Press-and-hold and swipe gestures cancel pending taps when recognized. Small
finger movement within the tap threshold does not reveal controls early.

A 500 ms hold temporarily previews 2× speed and shows only the speed readout,
without waking the OSD. Releasing restores the stored playback speed and leaves
OSD visibility unchanged.

Implementation and headless input coverage: `crates/ui/player.slint` and
`tests/{player_gestures,player_double_tap}.rs`.

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

The banner does not steal keyboard focus when it appears. Arrow navigation
reaches its source and dismiss actions. Back closes an open player menu first,
then dismisses the banner, then closes the player on a subsequent press.

Implementation: `src/app/next_episode.rs` owns eligibility and session guards;
`src/app/playback.rs` observes progress; `src/app/detail.rs` shares ID-based
episode stream selection with the existing picker; `src/app/run.rs` wires
callbacks and close cleanup. `crates/ui/player.slint` renders the banner through
the AppWindow `next_episode_*` properties/callbacks.

Regression coverage lives beside the policy in `src/app/next_episode.rs` and
in `tests/next_episode_banner.rs` for headless layout, navigation and input.

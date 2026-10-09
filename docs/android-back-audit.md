# Android Back audit

Focus recovery and remaining device checks for Android Back.

## Fixed and covered by regression tests

Android can finish the activity when Slint rejects both synthetic Back events.
The window capture scope depends on a surviving focused item: removing a
focused input or button can leave Back without a capture chain.

- Returning from Detail restores Home navigation focus on the next tick, after
  the outgoing Detail scope is removed, keeping row-grid Back inside the app.
- Leaving the Tracking tab restores Detail navigation focus. Adjust cancellation
  and suggestion reloads focus the persistent Tracking panel scope.
- Leaving episode search for another tab or an episode's streams clears its edit
  state and restores Detail navigation focus.
- Settings subpage closes restore the persistent navigation scope before the
  outgoing page is removed.
- Discover retains one search field across narrow/wide layouts, preserving its
  focus and query during rotation.
- Settings restores navigation focus when Addons, Categories, or Torrents rebuild
  their responsive editors. The rotation regression specifically exercises the
  addon URL; Tracking's account input is also checked across rotation.

The tests dispatch actual Back press/release events, including held repeats,
and verify acceptance and one-layer navigation. Home root still backgrounds the
app through the existing callback.

## Remaining audit items

These are candidates to investigate, not reproduced bugs:

- In `TrackingSettings` / `AccountEditor` (`crates/ui/tracking.slint`), check
  focus after confirming a local reset, finishing/cancelling sign-in, and
  replacing the account model. Conditional controls or account rows can disappear;
  verify that a surviving control owns focus before the next Back press.
- Add focused rotation coverage for Settings' category-name and torrent-folder
  inputs, including keyboard edit mode. Their responsive focus recovery is
  implemented, but the new regression directly checks only the addon URL.
- Verify physical Back and gesture Back on the S25 FE, including keyboard open,
  rapid presses, app background/resume, and rotation. The current verification
  is headless; physical-device behavior still needs verification.

Tests that inspect built-in input metadata use the default compiled UI, matching
Android releases. Those tests are disabled with `live-preview`, whose built-in
interpreter widgets do not expose the metadata needed by their focus fixtures.

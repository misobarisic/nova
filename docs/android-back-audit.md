# Android Back audit

Focus recovery and remaining device checks for Android Back.

## Earlier fixes with regression coverage

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

## Tracking focus and nested Back

Compiled-UI regressions cover More dismissal before Tracking closure, Tracking
closure after reopening, alignment cancellation, and focus recovery after
suggestion reloads. Device confirmation is still needed for entry editing,
unlink, and sign-in lifecycle changes.

- Entry editing focuses the persistent More / Back to review button both when
  entering and leaving the editor. Back discards unsaved inputs through the same
  state change as Cancel.
- The Tracking panel owns its active entry by stable ID. Confirmation, editor,
  and More layers dismiss before manual search, episode alignment, and Tracking
  itself. Detail's toolbar, keyboard, and system Back paths share this UI handler.
- Unlink focuses the persistent panel before submitting the action. Linked-card
  removal, model replacement, and row identity changes recover that focus;
  ordinary updates to an existing row do not move it.
- Sign-in actions and reset move focus off temporary controls before submission.
  Completion, cancellation, expiry, and disconnect restore the surviving settings
  scope. Per-service input focus replaces increment/decrement accounting, and
  disappearing inputs or replaced accounts clear stale focus state.
- The existing Settings keyboard-dismissal path and the window's held-Back repeat
  protection remain in place.

## Device verification

Check both navbar and gesture Back on the S25 FE:

- Open Edit tracker entry and press Back before touching any input. Repeat with
  unsaved edits; they should be discarded without saving to the tracker.
- Unlink a release, wait for its card to disappear, then press Back.
- Focus a sign-in input, finish or cancel sign-in, then press Back. Also check
  expiry, disconnect, reset confirmation/cancellation, and account replacement.
- Open each confirmation, editor, More panel, manual search, and alignment. Each
  press should close just the nearest layer.
- Check category-name and torrent-folder editing across rotation, rapid presses,
  app background/resume, and return from playback, including keyboard edit mode.
- From My Library, open a series, select an episode and open its streams. Navbar
  Back should close stream actions, return to Episodes, then return to My Library.
- Only Home root should background the app; returning should retain its state.

Tests that inspect built-in input metadata use the default compiled UI, matching
Android releases. Those tests are disabled with `live-preview`, whose built-in
interpreter widgets do not expose the metadata needed by their focus fixtures.

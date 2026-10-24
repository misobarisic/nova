# Last time…

Continue Watching keeps its direct Play action. After thirty days without recorded
playback of a show, its card adds **Last time…**. The same action remains in the
card menu for recent shows. The reminder is an optional sheet, never a playback
interruption.

The sheet follows Nova's shared page-motion controls: a 260 ms slide and a
220 ms scrim fade, gated by both the master animation and transition switches.
Close retains an immutable snapshot of the painted reminder, including its
confirmation mode, until the exit completes. The outgoing surface consumes
pointer, keyboard and Android system Back input; focus returns to the current
Home, selector or player only after its final frame. Rapid reopen reverses the
existing exit rather than starting another timer. Disabling either motion switch
finishes a pending exit immediately. Profile invalidation clears all text and
paint immediately, and navigation away from an open sheet cancels it without
carrying its synopsis into the next destination.

The profile's synced viewing journal supplies the actual last playback date and
the episode most recently completed in viewing chronology. It does not pick the
highest episode number, use mutable progress timestamps as dates, or treat a
manual watched flag as a completed playback session. A separate resume line gives
only episode numbering and progress for an unfinished episode. The only episode
title and synopsis shown belong to that recorded last completion; later episode
artwork, titles and synopses are never loaded into the reminder.

A completion means Nova observed playback at or beyond 90%, with the journal's
minimum engagement requirement. It does not prove every scene was seen. Unknown
history, unreadable sessions, or an uncertain historical rewatch run omit the
synopsis. A missing exact episode overview gets an explicit unavailable message.
New recorded rewatch runs mark their boundaries so an old run cannot supply a
later-episode synopsis. Metadata and journal scans run on workers, guarded by the
selected profile activation and reminder request generation.

**Replay ending…** opens the ordinary exact-episode source selector, including
matching completed downloads. The replay is a transient request scoped to that
selector opening, profile activation and episode. It has no normal playback
target: playback progress, watched status, Continue Watching, journal completion,
tracking and automatic successor handling do not advance during a brief replay.
Browser/external-player and casting destinations cannot own an isolated replay.

Playback loads paused. If the selected version matches the recorded source/file
fingerprint and runtime (within the existing five-second recovery tolerance), Nova
seeks to the final four minutes. Older sessions or different versions require an
explicit confirmation using the loaded version's actual runtime. Short or unknown
runtimes cannot trigger an ending seek. Cancelling confirmation closes playback.
Only opaque source/file fingerprints and runtime accompany new journal sessions;
no resolved links, headers or source credentials are stored. Existing session JSON
remains readable through optional fields; no sync wire or ALPN change is needed.

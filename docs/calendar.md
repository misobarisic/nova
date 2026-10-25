# Calendar

Calendar is a root destination in the desktop sidebar and the phone More page.
Home → Upcoming’s Calendar button opens this same full planner. Calendar and
Downloads use the same slide/fade entrance as Settings and About when opened
from More, respect both animation switches, and accept Back during entry.
It reads dated episode metadata already cached for library series; it does not
fetch a separate schedule service or persist a second copy of the library.
Movies and series marked On Hold or Dropped are excluded.

Next episode uses the same caught-up selection rule as Home Upcoming: released
dated episodes must all be watched before the earliest future episode appears.
Unlike Home's glance row, the planner does not cap the number of series.
All releases is the default and includes every valid dated episode in the cached
library metadata, including previous releases. Unknown or invalid release dates
never become invented calendar dates. Search filters series names. The header
counts releases in the displayed month or week, or the entire filtered schedule
in Timeline (including dates not loaded yet).

Month uses a Monday-first, fixed 42-day grid with seven equal-width columns
aligned across every week and the weekday headings. The Week strip uses the
same equal division; date digits, release counts, preview text and selection
borders do not change cell sizes. Today has an outline and the selected day
has an accent highlight. Layouts at least 1000 logical pixels wide show that
day's episodes and subsequent releases beside the grid; smaller layouts show
cards below it so all seven date columns remain readable.
Week shows seven days and grouped release cards. Timeline is a continuous dated
list, without month boundaries or Previous/Next month buttons. It initially
starts at the selected day (today on first entry); scrolling near the bottom
loads another 60 releases, crossing empty months and years. Earlier releases
adds the previous 60 while preserving the visible card's position; scroll up
to read them. Initial selection, Today and the day outline use the device’s local
civil date, including near UTC day/month boundaries. Search/scope/view changes
reset the loaded window. These transient limits are not persisted. Artwork
remains viewport-driven. Month and Week retain their Previous/Next controls.
Next episode retains Home's future-only, caught-up semantics.
Release cards always use show posters in Home's 2:3 proportions rather than
episode thumbnails, independently of Home's episode artwork setting.

Each release carries its media type, series ID, and
episode ID; opening a card revalidates those identifiers and opens real series
details. Series details' top-right options menu includes Remind me, also
available in the library menu. Each show offers Off, New episodes or Full season
released; the choice follows that profile's synced subscription. Existing
subscriptions retain New episodes. Full season includes weekly seasons once the
finale has released, as well as seasons released together.
Calendar → Manage reminders / Export calendar manages subscriptions and exports.
Subscriptions follow typed title identities and sync separately for each profile.
On Hold/Dropped or removed library titles retain a paused subscription; explicit
unsubscribe removes it. Device delivery starts disabled and stays local (an in-app digest on
desktop, OS notifications on Android). Watched or restricted/disallowed releases never enter the digest.

Implementation: `crates/ui/calendar.slint`, `src/app/calendar.rs`, and shared
`AppWindow` properties/callbacks in `crates/ui/appwindow.slint` wired by
`src/app/run.rs`. Date arithmetic and next-episode selection reuse Home helpers;
Home Upcoming’s Calendar shortcut is wired to the full planner; its episode
grid and next-episode selection remain unchanged.


The device digest is eligible from 09:00 local civil time, with a missed previous
release day eligible until the following 09:00. Metadata arriving late updates
that day's digest quietly. Durable episode receipts survive process restarts,
timezone/DST changes and corrected dates; a delivered episode does not alert
again when metadata moves it. The reminder panel shows refresh state and the
latest successful refresh time. Subscribed and smart-offline titles join the
bounded library metadata refresh even when they have never been watched.

Full-season delivery requires explicit provider evidence: a positive expected
season episode count, or an explicit finale marker identifying that season's
last episode. Nova then requires every numbered episode from 1 through that
count, a valid release date for every episode, and no future release (including
a future timestamp later today). Conflicting counts, gaps, duplicate episode
numbers and unknown dates keep the season pending. An Ended series status, a
later season or an entirely aired cached prefix alone does not establish that
the cached season is complete. Providers without completion evidence cannot
produce full-season reminders; their ordinary episode reminders and ICS exports
remain available.

The guarded episode-cache publication retains explicit `seasons` entries with
`season_number`/`seasonNumber`/`season` and `episode_count`/`episodeCount`, or
video extensions `seasonEpisodeCount`, `season_episode_count`,
`expectedSeasonEpisodeCount`, `isSeasonFinale`, `seasonFinale`,
`is_season_finale` and finale `episodeType`/`episode_type`, under
`Video.extra.novaSeasonCompletion`. Default Cinemeta metadata generally lacks
these explicit completion hints, so Nova does not guess a season's final count.
Evidence is retained
only when the original and canonical episode identities and numbering agree,
so split or renumbered provider seasons cannot donate a misleading count.
Existing caches acquire these hints during their next metadata refresh.
Season delivery has its own durable receipt, independent of episode receipts.
Open and Android taps still use the first unwatched real episode in that season.
Changing the reminder mode does not change the per-episode ICS export.

Android requests notification permission when device delivery is enabled. Denial
leaves subscriptions and calendar export usable. A persisted 15-minute
JobScheduler job runs bounded metadata refresh/delivery passes without an
Activity or foreground-service start. It recomputes local civil time on each
pass after boot, time changes and timezone changes; no exact-alarm permission is
needed. Android notification taps retain profile/title/episode identities and
pass profile selection, PIN and title policy before details open. Cancelled PIN
entry discards the pending tap. Shade dismissal and notification taps persist
acknowledgments, so later jobs cannot resurrect those episodes. Quiet corrections
use API-26-compatible group alert suppression even if Android creates a new
notification after the previous one was dismissed. The job is scheduled only
while a live profile has device delivery enabled; disabling/importing/removing
the final enabled owner cancels it.

Desktop reminders appear only in Calendar's in-app digest while Nova runs,
with a permitted missed digest on launch. Android is the only platform that
posts OS notifications. No desktop background daemon or native notification
adapter is installed. Open/Dismiss remain large focusable in-app actions on
phone, desktop and TV. The show list supports search and renders at most 50
matches at once; search can find the remaining titles.

Export scope is subscribed shows, library shows, the Calendar search filter or
explicitly selected shows. The start/end dates default to today through 90 days
ahead; invalid or reversed dates fail before saving. Preview reports series,
releases, omitted undated episodes and cancellation records. Save / Share .ics
uses the desktop save dialog or Android document picker. The saved export
namespace and event ledger live at `calendar_export:v1`, are included in the
reminders backup component, and keep UIDs stable after profile remapping.
Delivery receipts at `release_delivery:v1` stay device-local and are excluded
from backups. Export update reuses the saved namespace and scope; changing only
the range does not claim a provider cancellation. Disappearance within the
export's scope emits a higher-sequence cancelled event, while semantic edits
advance SEQUENCE once. Confirmed unambiguous episode aliases retain the old UID;
unproven IDs remain distinct.

ICS output uses CRLF, UTF-8-safe 75-octet folding, RFC 5545 escaping, stable
SHA-256 UIDs, DTSTAMP/SEQUENCE/LAST-MODIFIED and exclusive all-day end dates.
Offset-qualified timestamps become UTC events without an invented duration;
naive timestamps remain all-day dates. File exports are snapshots. They do not
edit an external calendar automatically. Import into a dedicated calendar and
replace its snapshot when an importer duplicates UID updates or ignores
cancellations.

Implementation additions: `src/app/release_reminders.rs` owns subscriptions,
digests and preview/ledger generation;
`release_reminders_android.rs`, `NovaReleaseJobService.java` and the private
`NovaReleaseDismissReceiver.java` own Android job, permission, tap and dismissal
glue. The packager receiver metadata extension is
`android/cargo-apk2-receivers.patch` (registered in `flake.nix`). Both jobs and UI use explicit `ProfileStore` ownership,
shared `headless_profile.rs` metadata access and profile-policy checks.

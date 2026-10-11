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
counts releases in the displayed month, week, or Agenda period.

Month uses a Monday-first, fixed 42-day grid. Today has an outline and the
selected day has a purple highlight. Desktop shows that day's episodes and
subsequent releases beside the grid; narrow layouts show cards below it.
Week shows seven days and grouped release cards. Agenda initially starts today
and groups a rolling month of releases chronologically: the start is included
and the same day next month is excluded. Previous and Next move that start by
one month, retaining the day number where possible and clamping to the month's
last day (January 31 becomes February 28, or 29 in a leap year). Today restores
today in every view. Month and Week also support browsing previous releases.
Next episode retains Home's future-only, caught-up semantics.

Each release carries its media type, series ID, and
episode ID; opening a card revalidates those identifiers and opens real series
details. Reminder and export controls are omitted because no platform reminder
or calendar export subsystem exists.

Implementation: `crates/ui/calendar.slint`, `src/app/calendar.rs`, and shared
`AppWindow` properties/callbacks in `crates/ui/appwindow.slint` wired by
`src/app/run.rs`. Date arithmetic and next-episode selection reuse Home helpers;
Home Upcoming’s Calendar shortcut is wired to the full planner; its episode
grid and next-episode selection remain unchanged.

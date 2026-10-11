# Calendar

Calendar is a root destination in the desktop sidebar and the phone More page.
It reads dated episode metadata already cached for library series; it does not
fetch a separate schedule service or persist a second copy of the library.
Movies and series marked On Hold or Dropped are excluded.

Next episode uses the same caught-up selection rule as Home Upcoming: released
dated episodes must all be watched before the earliest future episode appears.
Unlike Home's glance row, the planner does not cap the number of series.
All scheduled includes every library episode dated today or later. Unknown
release dates never become invented calendar dates. Search filters series names.

Month uses a Monday-first, fixed 42-day grid. Today has an outline and the
selected day has a purple highlight. Desktop shows that day's episodes and
subsequent releases beside the grid; narrow layouts show cards below it.
Week shows seven days and grouped release cards. Agenda groups the filtered
schedule chronologically. Each release carries its media type, series ID, and
episode ID; opening a card revalidates those identifiers and opens real series
details. Reminder and export controls are omitted because no platform reminder
or calendar export subsystem exists.

Implementation: `crates/ui/calendar.slint`, `src/app/calendar.rs`, and shared
`AppWindow` properties/callbacks in `crates/ui/appwindow.slint` wired by
`src/app/run.rs`. Date arithmetic and next-episode selection reuse Home helpers;
the existing Home Upcoming grid/calendar behavior is unchanged.

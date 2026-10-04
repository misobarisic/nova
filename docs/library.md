# My Library — behavior reference

How entries, buckets, episode counts, badges, categories and menus behave.
Derived state (buckets, badges, episode counts) is always computed locally from
library entries + playback progress + the cached episode list — nothing about
it is stored or synced. Code: `src/app/library.rs`, `src/app/episodes.rs`
(pure helpers, unit-tested in `src/app/tests.rs`), UI in `crates/ui/library.slint`.

## Terms

- **Dated** episode: carries a parseable `YYYY-MM-DD` air date. **Dateless**:
  none (e.g. specials, TBA listings).
- **Released** (`episode_is_out`): air date is today or past — or there is no
  usable date at all (dateless/unparseable count as released: only a *known
  future* date holds an episode back).
- **Unaired**: known air date in the future. Included in the card’s full
  episode total, but excluded from “N left” counts and never auto-marked or offered — but completion requires every known episode
  watched, so an unaired tail blocks Completed.
- **Watched**: sticky per-episode flag. Set automatically at ≥ 90% of known
  duration (`WATCHED_FRACTION`) or on natural end, or manually. Once set,
  position/duration are kept so un-toggling restores the resume rail.
- **Resumable**: not watched, position ≥ 10 s (`RESUME_MIN_SECS`), below 90%
  (unknown duration resumes past the 10 s cold-open window).

## Entries

- **Add**: detail-page bookmark. Stores id, type, name, year, poster/backdrop
  URLs, genres, description, plus empty categories, `Auto` status and an
  added timestamp.
- **Remove**: library card menu or the same bookmark. Only the entry is
  deleted — progress, season/episode history and Continue-Home removals are
  kept. Re-adding replaces the stored entry in place and refreshes its added
  timestamp, categories and status pin; progress-derived badges
  come back on their own. An open detail modal of that entry flips its
  bookmark off.
- **Order**: Recently Added (newest first) by default, with Title and Release
  year alternatives. Title search combines with the category filter. Actions
  resolve against this same filtered/sorted view.
- **Storage**: JSON `library` key in redb, synced per entry (`library`
  domain). Missing/corrupt reads as empty.
- **Opening** an entry shows the detail modal; reopening the same entry
  restores its tab/season/focus snapshot. Entering the page always prefetches
  episode metadata (not gated by the Discover prefetch toggle).

## Automatic buckets (the filter bar)

The horizontal pill order is fixed: **All, Watching, Completed, On Hold,
Dropped, Plan to Watch**, then user categories. Labels are localized, values stay English,
so a translation can never leak into the data.

| Bucket | Rule |
|---|---|
| Plan to Watch | No progress record at all (untouched movies, added-but-never-played series). |
| Watching | Any progress, not Completed. |
| Completed | **Every known episode watched.** Unaired episodes can never be watched, so a series with episodes still to come never completes. Needs a cached episode list. |
| On Hold / Dropped | Pinned manually per entry (card menu → status); overrides the derived bucket. Back-to-automatic clears the pin. |

Pinned entries appear under their pin only — never under the three auto
buckets. A pin replaces the card’s status pill, while resume text and the
watched/total rail remain derived from progress.

Quirk, documented as-is: movies have no episode list, so a fully watched
movie buckets as Watching (not Completed) and has no episode rail — it reads
`✓ Seen` instead (see below).

## Card anatomy: status, watched/total rail and resume text

Cards show the localized bucket as an artwork pill, an overflow button, a
single-line title, year/type metadata and optional playback text. The small
rail and “watched / total” count appear only when at least one known episode
is marked watched. The total includes the whole cached list (specials,
dateless and announced episodes); partial playback and stale progress for
unknown episode IDs do not inflate the count. Movies and unknown totals have
no episode rail. Completed series show their status and a full rail.

The **playback text** below still counts dated, released episodes only;
dateless episodes do not feed “N left” and unaired ones are tallied separately.
Resume (latest-updated resumable position) beats that text count.

| Situation | Badge | Bucket |
|---|---|---|
| Fresh series, nothing watched | (none — no noise) | Plan to Watch |
| Partially watched | `N left` (+ `· M unaired`) | Watching |
| Mid-episode (resume available) | `▶ Resume <Title>` (+ tail) | Watching |
| All dated released watched, unaired tail | `Caught up · N unaired` | Watching |
| All dated watched, untouched dateless pending, no tail | `Caught up` | Watching |
| All dated watched, started dateless episode | `▶ Resume <Title>` | Watching |
| Everything watched, nothing unaired | (none — status and full rail) | Completed |
| Nothing dated released, unaired known | `N unaired` | Plan to Watch if untouched, else Watching |
| Only untouched dateless episodes | (none) | Plan to Watch |
| Started dateless-only list | `▶ Resume <Title>` | Watching |
| No episode list cached, resume exists | `▶ Resume` | Watching |
| No episode list cached, something watched | `✓ Seen` | Watching |
| Movie in progress | `▶ Resume` | Watching |
| Movie watched | `✓ Seen` | Watching (never Completed) |
| Movie untouched | (none) | Plan to Watch |
| Pinned On Hold / Dropped | playback text as derived | under the pin |

A single-episode toggle *can* mark an unaired episode watched; once every
known episode is (however it got there), the series completes and the badge
goes silent.


## Card menu

The card’s overflow button opens a native popup on desktop and the page-level
bottom sheet on touch. Desktop right-click also pops the menu at the cursor;
touch hold or right-click opens the same bottom sheet, whose Cancel row closes it.
Rows: **Open** (same as tapping the card), **Mark series as
watched/unwatched** (label flips with state), **Mark as On Hold / Dropped /
Back to automatic**, **Remove from library**. All indices resolve against the
currently filtered/sorted view. A touch sheet closes if a background refresh
replaces its row, preventing its old index from acting on a different show.

## Marking rules

- Bulk mark-watched (series, season, this-and-previous) touches **released**
  episodes only — dateless count as released, unaired are skipped (they
  surface on Home → Upcoming).
- Marking unwatched clears the flag *and* the saved position (back to
  fresh) on every record it touches, and stamps an explicit unwatch intent
  for the sync merge. Library "mark unwatched" touches everything, unaired
  records included.
- Single-episode toggle/mark hits any episode, aired or not.
- "Clear resume position" zeroes the position but keeps a watched flag.
- Marking watched triggers auto-delete of that episode's downloads when
  enabled in Settings → Downloads.
- Every change persists progress and refreshes badges in place (posters keep
  loading), plus Home rows and category filters.

## User categories

- Created/renamed/removed in Settings → Categories. Names are trimmed;
  built-in bucket names are reserved and rejected.
- Assigned in the detail-page picker (tag button, visible for library entries
  once at least one user category exists), with an assignment-count badge.
- Filtering uses the same horizontal pill rail as the automatic buckets
  (localized labels / stored English bucket identifiers).
- Deleting a category strips it from every entry; if it was the active
  filter, the filter resets to All.

## Grid & navigation

- Scrolling down moves the title, item count and description out of view.
  Search, category filters, sorting and grid/list controls move to the top
  and stay pinned; returning to the top restores the hero. A fixed scroll
  viewport and header placeholder avoid touch-scroll jitter.
- Search opens/closes with the same expansion and fade durations as Detail’s
  episode search. Focus moves into the input after 80 ms; motion-disabled
  settings make the expansion immediate. On narrow screens the field opens
  on its own row below the sort/view/search-icon row; wide layouts keep it
  above the filters.
- Column floor comes from Settings → Display (min columns), applied live.
  Grid/list selection, search visibility/query and sort order survive detail
  navigation in AppWindow; these controls do not add persisted settings.
- Posters paint from the decoded cache, placeholders until loaded.
  Badge/progress updates patch rows in place — no model rebuild, so loading
  posters are never interrupted. If progress changes filter membership, the
  model is rebuilt so displayed indices keep matching the visible entries.
- Backing out of an entry restores the absolute scroll offset; a fully hidden
  focused card is revealed, a partially visible one is left alone.
- Keyboard zones cover filter bar + grid; focus survives the modal
  round-trip (state lives on `AppWindow`).
- Empty-hint text when there are no entries (or none pass the filter).

## Detail interplay

- Season cards show ✓ + watched fraction over **all** known season episodes
  (unaired included), so a currently airing season never reads full.
- Episode rows show per-episode ✓, resume rails and air dates; bulk
  watched-toggles there skip unaired, single toggles don't.

## Sync

Entries sync whole (assignments and status pins ride along), progress and
category names sync as their own domains, and the bucket/badge/checkmark
trio is re-derived from them on every device, so two devices with the same
records always agree.

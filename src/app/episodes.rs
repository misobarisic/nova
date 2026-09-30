//! Pure episode/progress helpers: ordering, labels, badges, filters.
use super::*;

/// Map key for one episode's progress entry.
pub(crate) fn progress_map_key(series_id: &str, episode_id: &str) -> String {
    format!("{series_id}\u{1}{episode_id}")
}
/// Days since the Unix epoch for an "YYYY-MM-DD" (prefix) date.
/// Same calendar math as the relative branch of
/// [`Bridge::format_human_date`]; `None` when unparseable.
pub(crate) fn iso_days(iso: &str) -> Option<i64> {
    let s = iso.trim();
    if s.len() < 10 {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: i64 = s[5..7].parse().ok()?;
    let day: i64 = s[8..10].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut days =
        (year - 1970) * 365 + (year - 1969) / 4 - (year - 1901) / 100 + (year - 1601) / 400;
    let mdays: &[i64] = &[0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    days += mdays[(month - 1) as usize] + (day - 1);
    if month > 2 && (year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)) {
        days += 1;
    }
    Some(days)
}
/// Today as days since the Unix epoch (like
/// [`Bridge::format_human_date`]).
pub(crate) fn today_days() -> i64 {
    (now_secs() / 86400) as i64
}
/// Whether an episode is considered released: dateless or unparseable
/// dates count as available (only a known future air date holds it back).
/// Bulk mark-watched actions (season / series / up-to-here) skip episodes
/// this returns false for, so unaired episodes never get marked.
pub(crate) fn episode_is_out(v: &Video, today: i64) -> bool {
    v.released
        .as_deref()
        .and_then(iso_days)
        .map(|d| d <= today)
        .unwrap_or(true)
}
/// Whether an episode carries a known air date at all. Surfacing paths
/// (Continue Watching's next-up offer, the library "N left" count, the Home
/// → Upcoming caught-up check) ignore dateless episodes: with no schedule
/// there is nothing to count down to, so they never appear on their own. A
/// manually started dateless episode still resumes wherever progress
/// exists — starting it is the explicit signal the date cannot give.
pub(crate) fn episode_has_air_date(v: &Video) -> bool {
    v.released.as_deref().and_then(iso_days).is_some()
}
/// Fraction watched (`0..1`, 0 when the duration is unknown).
pub(crate) fn progress_fraction(position_secs: f64, duration_secs: f64) -> f32 {
    if duration_secs <= 0.0 || position_secs <= 0.0 {
        return 0.0;
    }
    ((position_secs / duration_secs) as f32).clamp(0.0, 1.0)
}
/// Whether `pos / dur` counts as watched (duration must be known).
pub(crate) fn is_watched_position(pos: f64, dur: f64) -> bool {
    dur > 0.0 && pos > 0.0 && pos / dur >= WATCHED_FRACTION
}
/// Whether a saved position is worth offering as a resume.
pub(crate) fn resumable_position(pos: f64, dur: f64, watched: bool) -> bool {
    !watched && pos >= RESUME_MIN_SECS && (dur <= 0.0 || pos / dur < WATCHED_FRACTION)
}
/// Library badge for one series: `""` when nothing to report, otherwise
/// `"▶ Resume <label>"`, `"N left"` and/or `"M unaired"`. A fully watched
/// series reports nothing here: its card already carries the top-right
/// checkmark. `episodes` are the known videos of the series (episode-cache
/// order); unknown totals fall back to resume-only badges. Released and
/// unaired episodes are tallied separately: a dated-complete series reads
/// "Caught up" (plus " · 2 unaired" when episodes are still to come), while
/// a fully watched one stays silent — the checkmark carries it.
pub(crate) fn library_badge_for(
    series_id: &str,
    episodes: &[Video],
    map: &HashMap<String, EpisodeProgress>,
) -> String {
    let is_ours = |p: &EpisodeProgress| p.series_id == series_id;
    if episodes.is_empty() {
        // No episode list: report a resume point when one exists.
        let mut best: Option<&EpisodeProgress> = None;
        for p in map.values().filter(|p| is_ours(p)) {
            if p.watched {
                continue;
            }
            if !resumable_position(p.position_secs, p.duration_secs, p.watched) {
                continue;
            }
            if best
                .map(|b| p.updated_at_secs > b.updated_at_secs)
                .unwrap_or(true)
            {
                best = Some(p);
            }
        }
        return match best {
            Some(p) => format!(
                "{} {}",
                text::tr("▶ Resume"),
                episode_context_label_for(&p.episode_id, episodes).unwrap_or_default()
            )
            .trim()
            .to_string(),
            None => {
                if map.values().any(|p| is_ours(p) && p.watched) {
                    text::tr("✓ Seen").to_string()
                } else {
                    String::new()
                }
            }
        };
    }
    let mut watched_count = 0usize;
    let mut resume: Option<(&Video, &EpisodeProgress)> = None;
    // Released and unaired episodes are tallied separately: a series with
    // episodes still to come never reads "✓ Seen" — it stays in Watching
    // and feeds Home → Upcoming.
    let today = today_days();
    let mut unaired = 0usize;
    // Dated, released episodes only: dateless ones carry no schedule, so
    // they feed neither total nor count (a started one can still win the
    // resume slot below — that progress is explicit, not automatic).
    let mut aired_total = 0usize;
    for v in episodes {
        if !episode_is_out(v, today) {
            unaired += 1;
            continue;
        }
        let dated = episode_has_air_date(v);
        if dated {
            aired_total += 1;
        }
        match map.get(&progress_map_key(series_id, &v.id)) {
            Some(p) if p.watched => {
                if dated {
                    watched_count += 1;
                }
            }
            Some(p)
                if resumable_position(p.position_secs, p.duration_secs, p.watched)
                    && resume
                        .map(|(_, cur)| p.updated_at_secs > cur.updated_at_secs)
                        .unwrap_or(true) =>
            {
                resume = Some((v, p));
            }
            _ => {}
        }
    }
    let unaired_suffix = || {
        if unaired > 0 {
            text::unaired(unaired, true)
        } else {
            String::new()
        }
    };
    if aired_total == 0 {
        // Nothing dated released yet: a resume in progress still wins (the
        // user is mid-episode and "▶ Resume" beats silence), otherwise only
        // worth a badge when unaired episodes are known.
        if let Some((v, _)) = resume {
            return format!(
                "{} {}{}",
                text::tr("▶ Resume"),
                episode_context_label(v),
                unaired_suffix()
            );
        }
        return if unaired > 0 {
            text::unaired(unaired, false)
        } else {
            String::new()
        };
    }
    if watched_count >= aired_total {
        // Everything dated is seen — but a resume in progress still wins:
        // the user is mid-episode (possibly a dateless one), and "▶ Resume"
        // is more useful than silence. Otherwise a fully seen series carries
        // the top-right checkmark on its library card, so the badge stays
        // empty instead of repeating "✓ Seen". A dated-complete series that
        // is not fully seen — episodes still to come (unaired), or an
        // unwatched dateless episode pending — still names the wait.
        if let Some((v, _)) = resume {
            return format!(
                "{} {}{}",
                text::tr("▶ Resume"),
                episode_context_label(v),
                unaired_suffix()
            );
        }
        return if unaired > 0 || !series_fully_watched(series_id, episodes, map) {
            format!("{}{}", text::tr("Caught up"), unaired_suffix())
        } else {
            String::new()
        };
    }
    if let Some((v, _)) = resume {
        return format!(
            "{} {}{}",
            text::tr("▶ Resume"),
            episode_context_label(v),
            unaired_suffix()
        );
    }
    let left = aired_total - watched_count;
    // Only claim "N left" when something was actually watched; a fresh
    // series shows no badge instead of noise.
    if watched_count > 0 {
        format!("{}{}", text::left(left), unaired_suffix())
    } else {
        String::new()
    }
}
/// The next episode to offer after the user finishes one: the first dated,
/// released (see [`episode_is_out`]), not-yet-watched episode in canonical
/// season/episode order (extras last). Dateless episodes are skipped — with
/// no air date there is nothing to count down to, so they never surface on
/// their own (a manually started one still resumes). `None` when every
/// dated, released episode is watched — an unaired tail is Home → Upcoming's
/// job, not Continue Watching's. Used to surface the "next in line, not
/// started yet" card.
pub(crate) fn next_episode_to_watch<'a>(
    series_id: &str,
    episodes: &'a [Video],
    map: &HashMap<String, EpisodeProgress>,
) -> Option<&'a Video> {
    let today = today_days();
    for season in ordered_seasons(episodes) {
        for v in season_episodes(episodes, season) {
            if !episode_is_out(v, today) {
                continue;
            }
            if !episode_has_air_date(v) {
                continue;
            }
            let watched = map
                .get(&progress_map_key(series_id, &v.id))
                .is_some_and(|p| p.watched);
            if !watched {
                return Some(v);
            }
        }
    }
    None
}
/// Split a series' episode list for the Home → Upcoming caught-up check:
/// future (index, air days) pairs plus available / available-watched tallies.
/// Dateless episodes are skipped entirely (unknown schedule — they neither
/// join the unaired tail nor hold back the caught-up check; only a manual
/// start surfaces them, through Continue Watching's resume).
pub(crate) fn upcoming_tally(
    episodes: &[Video],
    is_watched: impl Fn(&Video) -> bool,
    today: i64,
) -> (Vec<(usize, i64)>, usize, usize) {
    let mut future: Vec<(usize, i64)> = Vec::new();
    let mut available = 0usize;
    let mut available_watched = 0usize;
    for (i, v) in episodes.iter().enumerate() {
        let Some(days) = v.released.as_deref().and_then(iso_days) else {
            continue;
        };
        if days > today {
            future.push((i, days));
            continue;
        }
        available += 1;
        if is_watched(v) {
            available_watched += 1;
        }
    }
    (future, available, available_watched)
}
/// Whether every known episode of a series is marked watched. Unaired
/// episodes (known future air date) can never be watched, so a series
/// with episodes still to come never counts as fully watched — it stays
/// in Watching and feeds Home → Upcoming. Empty lists never count.
pub(crate) fn series_fully_watched(
    series_id: &str,
    episodes: &[Video],
    map: &HashMap<String, EpisodeProgress>,
) -> bool {
    !episodes.is_empty()
        && episodes.iter().all(|v| {
            map.get(&progress_map_key(series_id, &v.id))
                .is_some_and(|p| p.watched)
        })
}
/// Automatic bucket for a library entry: "Completed" (every known episode
/// watched — series with unaired episodes never complete), "Watching"
/// (any progress, not complete) or "Plan to Watch" (untouched, including
/// movies which carry no progress).
pub(crate) fn auto_bucket(
    series_id: &str,
    episodes: &[Video],
    map: &HashMap<String, EpisodeProgress>,
) -> &'static str {
    if series_fully_watched(series_id, episodes, map) {
        "Completed"
    } else if map.values().any(|p| p.series_id == series_id) {
        "Watching"
    } else {
        "Plan to Watch"
    }
}
pub(crate) fn episode_context_label_for(episode_id: &str, episodes: &[Video]) -> Option<String> {
    episodes
        .iter()
        .find(|v| v.id == episode_id)
        .map(episode_context_label)
}
/// Distinct seasons of `videos` in display order: numbered seasons (`>= 1`)
/// ascending first, then the season-0 extras/specials group last.
pub(crate) fn ordered_seasons(videos: &[Video]) -> Vec<u32> {
    let mut seasons: Vec<u32> = Vec::new();
    for v in videos {
        if let Some(s) = v.season
            && !seasons.contains(&s)
        {
            seasons.push(s);
        }
    }
    seasons.sort_unstable();
    if let Some(pos) = seasons.iter().position(|&s| s == 0) {
        let extras = seasons.remove(pos);
        seasons.push(extras);
    }
    seasons
}
/// The videos of one season, ordered by episode number (missing numbers
/// last, insertion order preserved for ties).
pub(crate) fn season_episodes(videos: &[Video], season: u32) -> Vec<&Video> {
    let mut episodes: Vec<&Video> = videos.iter().filter(|v| v.season == Some(season)).collect();
    episodes.sort_by_key(|v| v.episode_number().unwrap_or(u32::MAX));
    episodes
}
/// Whether an episode matches the detail-page filter box (case-insensitive
/// match against the row label, overview and episode number; empty filter
/// matches everything).
pub(crate) fn episode_matches_filter(v: &Video, filter: &str) -> bool {
    let needle = filter.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    if v.episode_number()
        .map(|n| {
            n.to_string() == needle || format!("ep {n}") == needle || format!("e{n}") == needle
        })
        .unwrap_or(false)
    {
        return true;
    }
    let haystacks = [
        episode_row_label(v).to_lowercase(),
        v.overview.clone().unwrap_or_default().to_lowercase(),
    ];
    haystacks.iter().any(|h| h.contains(&needle))
}
/// Display label for a season value: season 0 holds extras/specials.
pub(crate) fn season_label(season: u32) -> String {
    text::season_label(season)
}
/// Representative artwork for a season: the thumbnail of its first episode
/// carrying one (episode order). Used for the season picker cards.
pub(crate) fn season_thumb_url(videos: &[Video], season: u32) -> Option<String> {
    season_episodes(videos, season)
        .into_iter()
        .find_map(|v| v.thumbnail.clone())
}
/// Short badge for an episode card (e.g. "S1 E3"); falls back gracefully
/// when the addon omits season or episode numbers.
pub(crate) fn episode_badge(v: &Video) -> String {
    match (v.season, v.episode_number()) {
        (Some(s), Some(n)) => format!("S{s} E{n}"),
        (None, Some(n)) => format!("E{n}"),
        (Some(s), None) => format!("S{s}"),
        (None, None) => "EP".to_string(),
    }
}
/// Identifier for an episode without a usable title (e.g. "S1 E3").
/// Shared fallback for the row and context labels below.
pub(crate) fn episode_se_label(v: &Video) -> String {
    match (v.season, v.episode_number()) {
        (Some(s), Some(n)) => format!("S{s} E{n}"),
        (None, Some(n)) => format!("E{n}"),
        (Some(s), None) => format!("S{s}"),
        (None, None) => String::new(),
    }
}
/// Row label for one episode inside a season picker: just the title (the
/// S/E badge on the thumbnail already identifies the episode). Falls back
/// to the S/E identifier when the addon sent no title.
pub(crate) fn episode_row_label(v: &Video) -> String {
    // Collapse interior whitespace (addon titles occasionally contain
    // line breaks): the grid card reserves height per explicit line
    // break even when painting a single elided line.
    let title: String = v.label().split_whitespace().collect::<Vec<_>>().join(" ");
    if !title.is_empty() {
        return title;
    }
    let se = episode_se_label(v);
    if !se.is_empty() {
        return se;
    }
    text::tr("Episode").to_string()
}
/// Context label for a picked episode shown above its streams: just the
/// title (the top-bar pill and badges carry the S/E identifier). Falls back
/// to the S/E identifier when the addon sent no title.
pub(crate) fn episode_context_label(v: &Video) -> String {
    let title = v.label().trim().to_string();
    if !title.is_empty() {
        return title;
    }
    episode_se_label(v)
}

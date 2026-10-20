//! Library release planner. UI state is local; scheduled identities are never row indices.
use super::home::{
    cal_cells, civil_from_days, days_from_civil, month_first, next_upcoming_episode, upcoming_row,
    weekday_monday0,
};
use super::*;
use crate::{CalendarDay, CalendarRelease};

impl Bridge {
    pub(super) fn show_downloads_page(&self) {
        self.shared
            .lock()
            .unwrap()
            .downloads_page_refresh
            .invalidate();
        if let Some(app) = self.app() {
            app.set_show_home(false);
            app.set_show_library(false);
            app.set_show_settings(false);
            app.set_show_about(false);
            app.set_show_calendar(false);
            app.set_show_downloads(true);
        }
        self.refresh_downloads_page();
    }

    pub(super) fn show_calendar_page(&self) {
        if let Some(app) = self.app() {
            app.set_show_home(false);
            app.set_show_library(false);
            app.set_show_settings(false);
            app.set_show_about(false);
            app.set_show_downloads(false);
            app.set_show_calendar(true);
            if app.get_calendar_first() == 0 {
                app.set_calendar_first(month_first(today_days()) as i32);
                app.set_calendar_selected(today_days() as i32);
            }
        }
        self.apply_calendar_to_ui();
    }

    pub(super) fn calendar_step(&self, delta: i32) {
        let Some(app) = self.app() else { return };
        let selected = i64::from(app.get_calendar_selected());
        if app.get_calendar_view() == 1 {
            let day = selected + i64::from(delta) * 7;
            app.set_calendar_selected(day as i32);
            app.set_calendar_first(month_first(day) as i32);
        } else if app.get_calendar_view() == 2 {
            return;
        } else {
            let (year, month, _) = civil_from_days(i64::from(app.get_calendar_first()));
            let shifted = i64::from(month) - 1 + i64::from(delta);
            let first = days_from_civil(
                year + shifted.div_euclid(12),
                (shifted.rem_euclid(12) + 1) as u32,
                1,
            );
            app.set_calendar_first(first as i32);
            app.set_calendar_selected(first as i32);
        }
        self.apply_calendar_to_ui();
    }

    pub(super) fn calendar_today(&self) {
        if let Some(app) = self.app() {
            app.set_calendar_first(month_first(today_days()) as i32);
            app.set_calendar_selected(today_days() as i32);
        }
        self.calendar_reset();
    }

    /// Search, scope and view changes start a new bounded agenda window.
    pub(super) fn calendar_reset(&self) {
        if let Some(app) = self.app() {
            app.set_calendar_agenda_before(0);
            app.set_calendar_agenda_after(AGENDA_BATCH as i32);
            app.set_calendar_agenda_revision(app.get_calendar_agenda_revision().wrapping_add(1));
        }
        self.apply_calendar_to_ui();
    }

    pub(super) fn calendar_agenda_more(&self, direction: i32) {
        let Some(app) = self.app() else { return };
        if !app.get_show_calendar() || app.get_calendar_view() != 2 {
            return;
        }
        if direction < 0 && app.get_calendar_has_earlier() {
            app.set_calendar_agenda_before(
                app.get_calendar_agenda_before()
                    .saturating_add(AGENDA_BATCH as i32),
            );
        } else if direction > 0 && app.get_calendar_has_later() {
            app.set_calendar_agenda_after(
                app.get_calendar_agenda_after()
                    .saturating_add(AGENDA_BATCH as i32),
            );
        } else {
            return;
        }
        self.apply_calendar_to_ui();
    }

    pub(super) fn calendar_pick(&self, day: i32) {
        if let Some(app) = self.app() {
            app.set_calendar_selected(day);
        }
        self.apply_calendar_to_ui();
    }

    pub(super) fn calendar_release_picked(&self, key: SharedString) {
        let Ok((type_, series, episode)) = serde_json::from_str::<(String, String, String)>(&key)
        else {
            return;
        };
        let entry = self
            .shared
            .lock()
            .unwrap()
            .entries
            .iter()
            .find(|e| e.id == series && e.type_ == type_)
            .cloned();
        // Revalidate the episode against its series cache. A refreshed schedule
        // cannot redirect an old card to an unrelated row.
        if let Some(entry) = entry.filter(|_| {
            read_episodes_cache_for(&type_, &series)
                .is_some_and(|videos| videos.iter().any(|v| v.id == episode))
        }) {
            self.open_library_entry(entry);
            self.detail_tab_picked(3);
        }
    }

    pub(super) fn apply_calendar_to_ui(&self) {
        let Some(app) = self.app() else { return };
        if !app.get_show_calendar() {
            return;
        }
        let (entries, progress) = {
            let state = self.shared.lock().unwrap();
            (state.entries.clone(), state.progress.clone())
        };
        let today = today_days();
        let query = app.get_calendar_search().to_lowercase();
        let mut schedule = Vec::new();
        for e in &entries {
            if e.type_ == "movie"
                || matches!(e.watch_status, WatchStatus::OnHold | WatchStatus::Dropped)
                || !e.name.to_lowercase().contains(query.trim())
            {
                continue;
            }
            let videos = read_episodes_cache_for(&e.type_, &e.id).unwrap_or_default();
            let selected = calendar_releases(
                &videos,
                app.get_calendar_all(),
                |v| {
                    progress
                        .get(&progress_map_key(&e.id, &v.id))
                        .is_some_and(|p| p.watched)
                },
                today,
            );
            schedule.extend(
                selected
                    .into_iter()
                    .map(|(episode_id, air_days)| UpcomingEntry {
                        series_id: e.id.clone(),
                        type_: e.type_.clone(),
                        episode_id,
                        air_days,
                    }),
            );
        }
        schedule.sort_by(|a, b| {
            (a.air_days, &a.series_id, &a.episode_id).cmp(&(
                b.air_days,
                &b.series_id,
                &b.episode_id,
            ))
        });
        let first = i64::from(app.get_calendar_first());
        let day = i64::from(app.get_calendar_selected());
        let mut counts = HashMap::new();
        for item in &schedule {
            *counts.entry(item.air_days).or_insert(0) += 1;
        }
        let weekdays = [
            text::tr("Mon"),
            text::tr("Tue"),
            text::tr("Wed"),
            text::tr("Thu"),
            text::tr("Fri"),
            text::tr("Sat"),
            text::tr("Sun"),
        ];
        let cells = cal_cells(first, &counts, Some(day), today)
            .into_iter()
            .map(|cell| {
                let preview = schedule
                    .iter()
                    .filter(|s| s.air_days == i64::from(cell.epoch))
                    .take(2)
                    .filter_map(|s| {
                        entries
                            .iter()
                            .find(|e| e.id == s.series_id && e.type_ == s.type_)
                            .map(|e| e.name.clone())
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                CalendarDay {
                    weekday: weekdays[weekday_monday0(i64::from(cell.epoch)) as usize].into(),
                    cell,
                    preview: preview.into(),
                }
            })
            .collect::<Vec<_>>();
        let week_start = day - weekday_monday0(day) as i64;
        let period = calendar_period(first, day, app.get_calendar_view());
        let week = (0..7)
            .map(|offset| {
                let epoch = week_start + offset;
                let (_, _, date) = civil_from_days(epoch);
                CalendarDay {
                    cell: CalCell {
                        epoch: epoch as i32,
                        day: date as i32,
                        in_month: true,
                        count: counts.get(&epoch).copied().unwrap_or(0) as i32,
                        selected: epoch == day,
                        today: epoch == today,
                    },
                    preview: SharedString::default(),
                    weekday: weekdays[offset as usize].into(),
                }
            })
            .collect::<Vec<_>>();
        let retained = self
            .shared
            .lock()
            .unwrap()
            .page_artwork
            .retained_keys(super::page_artwork::Page::Calendar);
        let old_rows: HashMap<String, CalendarRelease> = app
            .get_calendar_rows()
            .iter()
            .map(|row| (format!("r{}", row.key), row))
            .chain(
                app.get_calendar_later()
                    .iter()
                    .map(|row| (format!("l{}", row.key), row)),
            )
            .collect();
        let mut artwork_identities = Vec::new();
        let mut rows = |items: Vec<&UpcomingEntry>, list: &str| {
            let mut previous = None;
            items
                .into_iter()
                .filter_map(|item| {
                    let row = upcoming_row(&entries, item, 0, false, true)?;
                    let group = if previous == Some(item.air_days) {
                        String::new()
                    } else {
                        calendar_date(item.air_days)
                    };
                    previous = Some(item.air_days);
                    let key =
                        serde_json::to_string(&(&item.type_, &item.series_id, &item.episode_id))
                            .unwrap_or_default();
                    let artwork_key = format!("{list}{key}");
                    let identity = super::page_artwork::Identity::new(
                        row.art_url.to_string(),
                        ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
                    );
                    let (poster, is_loaded) = if retained.contains(&artwork_key) {
                        if let Some(pixels) = decoded_cache_get(&identity.cache_key) {
                            (Image::from_rgba8(pixels), true)
                        } else if let Some(old) = old_rows
                            .get(&artwork_key)
                            .filter(|old| old.artwork_identity == identity.cache_key)
                        {
                            (old.poster.clone(), old.is_loaded)
                        } else {
                            (Image::default(), false)
                        }
                    } else {
                        (Image::default(), false)
                    };
                    let artwork_identity = identity.cache_key.clone().into();
                    artwork_identities.push((artwork_key, identity));
                    Some(CalendarRelease {
                        key: key.into(),
                        artwork_identity,
                        title: row.title,
                        subtitle: row.subtitle,
                        date: row.date,
                        group: group.into(),
                        poster,
                        is_loaded,
                    })
                })
                .collect::<Vec<_>>()
        };
        let agenda = agenda_window(
            &schedule,
            day,
            app.get_calendar_agenda_before().max(0) as usize,
            app.get_calendar_agenda_after().max(1) as usize,
        );
        app.set_calendar_has_earlier(agenda.start > 0);
        app.set_calendar_has_later(agenda.end < schedule.len());
        let visible = if app.get_calendar_view() == 2 {
            schedule[agenda].iter().collect()
        } else {
            schedule
                .iter()
                .filter(|item| {
                    if app.get_calendar_view() == 0 {
                        item.air_days == day
                    } else {
                        in_period(item.air_days, period)
                    }
                })
                .collect()
        };
        let visible_rows = rows(visible, "r");
        let later_rows = rows(
            schedule
                .iter()
                .filter(|item| item.air_days > day)
                .take(8)
                .collect(),
            "l",
        );
        self.shared.lock().unwrap().page_artwork.reconcile(
            super::page_artwork::Page::Calendar,
            format!(
                "{first}:{day}:{}:{}:{query}",
                app.get_calendar_view(),
                app.get_calendar_all()
            ),
            artwork_identities,
            app.get_page_artwork_allowed(),
        );
        app.set_calendar_rows(Rc::new(VecModel::from(visible_rows)).into());
        app.set_calendar_later(Rc::new(VecModel::from(later_rows)).into());
        app.set_calendar_cells(Rc::new(VecModel::from(cells)).into());
        app.set_calendar_week(Rc::new(VecModel::from(week)).into());
        let (year, month, _) = civil_from_days(first);
        app.set_calendar_title(
            if app.get_calendar_view() == 1 {
                text::calendar_week_title(
                    civil_from_days(week_start),
                    civil_from_days(week_start + 6),
                )
            } else if app.get_calendar_view() == 2 {
                text::tr("Timeline").into()
            } else {
                text::cal_month_title(month, year)
            }
            .into(),
        );
        app.set_calendar_day_title(calendar_date(day).into());
        app.set_calendar_count(
            schedule
                .iter()
                .filter(|item| app.get_calendar_view() == 2 || in_period(item.air_days, period))
                .count() as i32,
        );
        self.schedule_page_artwork(super::page_artwork::Page::Calendar);
        if app.get_show_calendar() {
            app.recover_back_focus();
        }
    }
}

const AGENDA_BATCH: usize = 60;

/// Page by release count, not calendar months: sparse schedules cross empty
/// months immediately, and dense dates cannot cause an unbounded first render.
fn agenda_window(
    schedule: &[UpcomingEntry],
    selected: i64,
    before: usize,
    after: usize,
) -> std::ops::Range<usize> {
    let anchor = schedule.partition_point(|item| item.air_days < selected);
    anchor.saturating_sub(before)..anchor.saturating_add(after).min(schedule.len())
}

fn calendar_date(day: i64) -> String {
    let (year, month, date) = civil_from_days(day);
    text::calendar_date(date, month, year)
}

/// Shift a local date by whole months, clamping dates such as January 31
/// to the last valid day of the destination month.
fn shift_month(day: i64, delta: i32) -> i64 {
    let (year, month, date) = civil_from_days(day);
    let shifted = i64::from(month) - 1 + i64::from(delta);
    let year = year + shifted.div_euclid(12);
    let month = (shifted.rem_euclid(12) + 1) as u32;
    let first = days_from_civil(year, month, 1);
    let next = if month == 12 {
        days_from_civil(year + 1, 1, 1)
    } else {
        days_from_civil(year, month + 1, 1)
    };
    first + i64::from(date).min(next - first) - 1
}

/// Half-open boundaries keep adjacent calendar periods free of duplicates.
fn calendar_period(first: i64, selected: i64, view: i32) -> (i64, i64) {
    match view {
        0 => (first, shift_month(first, 1)),
        1 => {
            let monday = selected - i64::from(weekday_monday0(selected));
            (monday, monday + 7)
        }
        _ => (i64::MIN, i64::MAX),
    }
}
fn in_period(day: i64, period: (i64, i64)) -> bool {
    day >= period.0 && day < period.1
}
fn calendar_releases(
    videos: &[Video],
    all: bool,
    watched: impl Fn(&Video) -> bool,
    today: i64,
) -> Vec<(String, i64)> {
    if all {
        videos
            .iter()
            .filter_map(|v| {
                v.released
                    .as_deref()
                    .and_then(iso_days)
                    .map(|day| (v.id.clone(), day))
            })
            .collect()
    } else {
        next_upcoming_episode(videos, watched, today)
            .map(|(index, day)| vec![(videos[index].id.clone(), day)])
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn date(year: i64, month: u32, day: u32) -> i64 {
        days_from_civil(year, month, day)
    }
    #[test]
    fn all_releases_include_past_dates_and_ignore_invalid_dates() {
        let videos: Vec<Video> = serde_json::from_value(serde_json::json!([
            {"id":"past", "released":"2026-09-12"},
            {"id":"future", "released":"2026-11-12T12:00:00Z"},
            {"id":"bad", "released":"2026-02-30"},
            {"id":"unknown"}
        ]))
        .unwrap();
        let today = date(2026, 10, 11);
        assert_eq!(
            calendar_releases(&videos, true, |_| false, today),
            vec![
                ("past".into(), date(2026, 9, 12)),
                ("future".into(), date(2026, 11, 12))
            ]
        );
        assert_eq!(
            calendar_releases(&videos, false, |_| true, today),
            vec![("future".into(), date(2026, 11, 12))]
        );
        assert!(calendar_releases(&videos, false, |_| false, today).is_empty());
    }
    #[test]
    fn month_steps_clamp_dates_across_leap_years_and_year_boundaries() {
        assert_eq!(shift_month(date(2026, 1, 31), 1), date(2026, 2, 28));
        assert_eq!(shift_month(date(2024, 1, 31), 1), date(2024, 2, 29));
        assert_eq!(shift_month(date(2026, 3, 31), -1), date(2026, 2, 28));
        assert_eq!(shift_month(date(2026, 1, 15), -1), date(2025, 12, 15));
        assert_eq!(shift_month(date(2026, 12, 15), 1), date(2027, 1, 15));
    }
    #[test]
    fn periods_have_inclusive_start_and_exclusive_end() {
        let selected = date(2026, 10, 11);
        let month = calendar_period(date(2026, 10, 1), selected, 0);
        assert_eq!(month, (date(2026, 10, 1), date(2026, 11, 1)));
        let week = calendar_period(date(2026, 10, 1), selected, 1);
        assert_eq!(week, (date(2026, 10, 5), date(2026, 10, 12)));
        for period in [month, week] {
            assert!(!in_period(period.0 - 1, period));
            assert!(in_period(period.0, period));
            assert!(in_period(period.1 - 1, period));
            assert!(!in_period(period.1, period));
        }
    }
    #[test]
    fn agenda_spans_months_and_years_in_bounded_nonduplicated_batches() {
        let selected = date(2026, 10, 15);
        let schedule = (0..200)
            .map(|index| UpcomingEntry {
                air_days: selected + (index - 80) * 40,
                episode_id: index.to_string(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let initial = agenda_window(&schedule, selected, 0, AGENDA_BATCH);
        assert_eq!(initial, 80..140);
        assert!(schedule[initial.end - 1].air_days > date(2027, 12, 31));
        let appended = agenda_window(&schedule, selected, 0, AGENDA_BATCH * 2);
        assert_eq!(appended, 80..200);
        let prepended = agenda_window(&schedule, selected, AGENDA_BATCH, AGENDA_BATCH * 2);
        assert_eq!(prepended, 20..200);
        assert_eq!(
            schedule[prepended.clone()]
                .iter()
                .map(|item| &item.episode_id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            prepended.len()
        );
        assert_eq!(
            agenda_window(&schedule, selected, usize::MAX, usize::MAX),
            0..200
        );
        assert_eq!(agenda_window(&[], selected, 60, 60), 0..0);
        assert_eq!(agenda_window(&schedule, date(9999, 1, 1), 0, 60), 200..200);
        assert_eq!(agenda_window(&schedule, date(9999, 1, 1), 60, 60), 140..200);
    }

    #[test]
    fn agenda_handles_dense_days_and_schedule_changes_without_index_state() {
        let selected = date(2026, 10, 15);
        let mut schedule = (0..130)
            .map(|index| UpcomingEntry {
                air_days: selected,
                episode_id: index.to_string(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        assert_eq!(agenda_window(&schedule, selected, 0, 60), 0..60);
        assert_eq!(agenda_window(&schedule, selected, 0, 120), 0..120);
        // Recompute the date anchor when metadata changes or a filter removes rows.
        schedule.insert(
            0,
            UpcomingEntry {
                air_days: selected - 30,
                ..Default::default()
            },
        );
        assert_eq!(agenda_window(&schedule, selected, 0, 60), 1..61);
        schedule.truncate(5);
        assert_eq!(agenda_window(&schedule, selected, 60, 120), 0..5);
    }
}

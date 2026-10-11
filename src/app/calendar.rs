//! Library release planner. UI state is local; scheduled identities are never row indices.
use super::home::{
    cal_cells, civil_from_days, days_from_civil, month_first, next_upcoming_episode, upcoming_row,
    weekday_monday0,
};
use super::*;
use crate::{CalendarDay, CalendarRelease};

impl Bridge {
    pub(super) fn show_downloads_page(&self) {
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
            let selected = if app.get_calendar_all() {
                videos
                    .iter()
                    .filter_map(|v| {
                        v.released
                            .as_deref()
                            .and_then(iso_days)
                            .filter(|day| *day >= today)
                            .map(|day| (v.id.clone(), day))
                    })
                    .collect::<Vec<_>>()
            } else {
                next_upcoming_episode(
                    &videos,
                    |v| {
                        progress
                            .get(&progress_map_key(&e.id, &v.id))
                            .is_some_and(|p| p.watched)
                    },
                    today,
                )
                .map(|(i, day)| vec![(videos[i].id.clone(), day)])
                .unwrap_or_default()
            };
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
        let rows = |items: Vec<&UpcomingEntry>| {
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
                    Some(CalendarRelease {
                        key: serde_json::to_string(&(
                            &item.type_,
                            &item.series_id,
                            &item.episode_id,
                        ))
                        .unwrap_or_default()
                        .into(),
                        title: row.title,
                        subtitle: row.subtitle,
                        date: row.date,
                        group: group.into(),
                        poster: row.poster,
                        is_loaded: row.is_loaded,
                    })
                })
                .collect::<Vec<_>>()
        };
        let visible = schedule
            .iter()
            .filter(|item| match app.get_calendar_view() {
                0 => item.air_days == day,
                1 => item.air_days >= week_start && item.air_days < week_start + 7,
                _ => true,
            })
            .collect();
        app.set_calendar_rows(Rc::new(VecModel::from(rows(visible))).into());
        app.set_calendar_later(
            Rc::new(VecModel::from(rows(
                schedule
                    .iter()
                    .filter(|item| item.air_days > day)
                    .take(8)
                    .collect(),
            )))
            .into(),
        );
        app.set_calendar_cells(Rc::new(VecModel::from(cells)).into());
        app.set_calendar_week(Rc::new(VecModel::from(week)).into());
        let (year, month, _) = civil_from_days(first);
        app.set_calendar_title(
            if app.get_calendar_view() == 1 {
                text::calendar_week_title(
                    civil_from_days(week_start),
                    civil_from_days(week_start + 6),
                )
            } else {
                text::cal_month_title(month, year)
            }
            .into(),
        );
        app.set_calendar_day_title(calendar_date(day).into());
        app.set_calendar_count(schedule.len() as i32);
        // Fetch only the displayed cards. Completion targets stable identities,
        // so changing the search or selected date cannot paint the wrong row.
        let mut requested = HashSet::new();
        for model in [app.get_calendar_rows(), app.get_calendar_later()] {
            for row in model.iter().take(80).filter(|row| !row.is_loaded) {
                if !requested.insert(row.key.clone()) {
                    continue;
                }
                let Ok((type_, series, episode)) =
                    serde_json::from_str::<(String, String, String)>(&row.key)
                else {
                    continue;
                };
                let Some(entry) = entries.iter().find(|e| e.type_ == type_ && e.id == series)
                else {
                    continue;
                };
                let videos = read_episodes_cache_for(&type_, &series).unwrap_or_default();
                let url = videos
                    .iter()
                    .find(|v| v.id == episode)
                    .and_then(|v| v.thumbnail.as_deref())
                    .filter(|url| !url.trim().is_empty())
                    .unwrap_or(&entry.poster_url)
                    .to_owned();
                if url.is_empty() {
                    continue;
                }
                let bridge = self.clone();
                let key = row.key;
                net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
                    let Some(pixels) = pixels else { return };
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(app) = bridge.app() else { return };
                        for model in [app.get_calendar_rows(), app.get_calendar_later()] {
                            for index in 0..model.row_count() {
                                if let Some(mut row) = model.row_data(index)
                                    && row.key == key
                                {
                                    row.poster = Image::from_rgba8(pixels.clone());
                                    row.is_loaded = true;
                                    model.set_row_data(index, row);
                                }
                            }
                        }
                    });
                });
            }
        }
    }
}

fn calendar_date(day: i64) -> String {
    let (year, month, date) = civil_from_days(day);
    text::calendar_date(date, month, year)
}

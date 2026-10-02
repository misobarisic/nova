//! My Library: entries, categories and persistence.
use super::*;

impl Bridge {
    pub(super) fn library_contains(&self, id: &str) -> bool {
        self.shared
            .lock()
            .unwrap()
            .entries
            .iter()
            .any(|e| e.id == id)
    }

    /// The library entries currently shown in the grid, i.e. `Shared.entries`
    /// filtered by the active category filter (empty = all). The order matches
    /// the Slint `library` model, so UI indices map 1:1 onto this list.
    pub(super) fn current_library_view(&self) -> Vec<LibraryEntry> {
        let filter = self
            .app()
            .map(|a| a.get_library_filter_category().to_string())
            .unwrap_or_default();
        let (entries, progress) = {
            let state = self.shared.lock().unwrap();
            (state.entries.clone(), state.progress.clone())
        };
        if filter.is_empty() {
            return entries;
        }
        if BUILTIN_FILTERS.contains(&filter.as_str()) {
            // Automatic buckets: pinned statuses win, the rest derive
            // from playback progress (episode lists come from cache).
            return entries
                .into_iter()
                .filter(|e| match filter.as_str() {
                    "On Hold" => e.watch_status == WatchStatus::OnHold,
                    "Dropped" => e.watch_status == WatchStatus::Dropped,
                    bucket => {
                        if e.watch_status != WatchStatus::Auto {
                            false
                        } else {
                            let episodes =
                                read_episodes_cache_for(&e.type_, &e.id).unwrap_or_default();
                            auto_bucket(&e.id, &episodes, &progress) == bucket
                        }
                    }
                })
                .collect();
        }
        entries
            .into_iter()
            .filter(|e| e.categories.iter().any(|c| c == &filter))
            .collect()
    }

    /// Refresh the Slint `library` model from `Shared.entries`.
    pub(super) fn apply_library_to_ui(&self) {
        let view = self.current_library_view();
        let map = self.shared.lock().unwrap().progress.clone();
        let cards: Vec<MediaCard> = view
            .iter()
            .map(|e| {
                let episodes = read_episodes_cache_for(&e.type_, &e.id).unwrap_or_default();
                // A pinned status (On Hold / Dropped) replaces the playback
                // badge; automatic entries show the progress badge.
                let badge = match e.watch_status.badge_label() {
                    // Display only: the stored variant keeps its English label.
                    Some(label) => SharedString::from(text::tr(label)),
                    None => SharedString::from(library_badge_for(&e.id, &episodes, &map)),
                };
                MediaCard {
                    id: e.id.clone().into(),
                    title: e.name.clone().into(),
                    year: e.year.clone().into(),
                    poster_path: e.poster_url.clone().into(),
                    poster: Image::default(),
                    is_loaded: false,
                    badge,
                    watched: series_fully_watched(&e.id, &episodes, &map),
                }
            })
            .collect();
        if let Some(app) = self.app() {
            app.set_library(Rc::new(VecModel::from(cards)).into());
        }
        self.dispatch_library_posters(&view);
    }

    /// Main thread: library card context-menu action. `index` is into the
    /// current (filter-narrowed) library view; `action`: 1 mark the whole
    /// series watched, 2 mark it unwatched. Marking watched only touches
    /// released episodes — unaired ones stay untouched (they surface on
    /// Home → Upcoming instead).
    pub(super) fn library_watch_action(&self, index: usize, action: i32) {
        let target = match action {
            1 => true,
            2 => false,
            _ => return,
        };
        let today = today_days();
        let (series_id, episode_ids) = {
            let view = self.current_library_view();
            let e = match view.get(index) {
                Some(e) => e,
                None => return,
            };
            let episodes = read_episodes_cache_for(&e.type_, &e.id).unwrap_or_default();
            let ids = if target {
                episodes
                    .iter()
                    .filter(|v| episode_is_out(v, today))
                    .map(|v| v.id.clone())
                    .collect::<Vec<_>>()
            } else {
                episodes.iter().map(|v| v.id.clone()).collect::<Vec<_>>()
            };
            (e.id.clone(), ids)
        };
        if episode_ids.is_empty() {
            return;
        }
        {
            let mut state = self.shared.lock().unwrap();
            for id in &episode_ids {
                Self::set_episode_watched_locked(&mut state.progress, &series_id, id, target);
            }
        }
        self.persist_and_refresh_progress();
    }

    /// Main thread: library card context-menu status action. `index` is
    /// into the current (filter-narrowed) library view; `status`: 0 back
    /// to automatic, 1 pin On Hold, 2 pin Dropped. Pins override the
    /// derived bucket (Plan to Watch / Watching / Completed).
    pub(super) fn library_status_action(&self, index: usize, status: i32) {
        let watch_status = match status {
            1 => WatchStatus::OnHold,
            2 => WatchStatus::Dropped,
            0 => WatchStatus::Auto,
            _ => return,
        };
        let changed = {
            let view = self.current_library_view();
            let entry_id = match view.get(index) {
                Some(e) => e.id.clone(),
                None => return,
            };
            let mut state = self.shared.lock().unwrap();
            match state.entries.iter_mut().find(|e| e.id == entry_id) {
                Some(e) if e.watch_status != watch_status => {
                    e.watch_status = watch_status;
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.persist_library();
            self.apply_library_to_ui();
        }
    }

    /// Persist the current in-memory library list to the KV store.
    pub(super) fn persist_library(&self) {
        let entries = {
            let state = self.shared.lock().unwrap();
            state.entries.clone()
        };
        write_persisted_library(&entries);
    }

    /// Detail-page "Add to library / Remove from library" toggle.
    pub(super) fn toggle_current_in_library(&self) {
        let (id, type_, name, year, poster_url, background_url, genres, description, currently_in) = {
            let state = self.shared.lock().unwrap();
            let m = match state.modal_item.as_ref() {
                Some(m) => m,
                None => return,
            };
            (
                m.id.clone(),
                m.type_.clone(),
                m.name.clone(),
                m.year.clone(),
                m.poster_url.clone(),
                m.background_url.clone(),
                m.genres.clone(),
                m.description.clone(),
                state.entries.iter().any(|e| e.id == m.id),
            )
        };

        {
            let mut state = self.shared.lock().unwrap();
            if currently_in {
                remove_library_entry(&mut state.entries, &id);
            } else {
                upsert_library(
                    &mut state.entries,
                    LibraryEntry {
                        id,
                        type_,
                        name,
                        year,
                        poster_url,
                        background_url,
                        genres,
                        description,
                        categories: Vec::new(),
                        watch_status: WatchStatus::Auto,
                        added_at_secs: now_secs(),
                    },
                );
            }
        }
        if let Some(app) = self.app() {
            app.set_in_library(!currently_in);
        }
        self.persist_library();
        self.apply_library_to_ui();
        self.sync_modal_category_flags();
    }

    /// Remove the library card at `index` (badge on the library grid). The
    /// index is into the *filtered* grid view (the `library` model), so it is
    /// resolved against `current_library_view`.
    pub(super) fn remove_library_at(&self, index: usize) {
        let view = self.current_library_view();
        let removed_id = {
            let mut state = self.shared.lock().unwrap();
            let id = view.get(index).map(|e| e.id.clone());
            if let Some(id) = id.as_deref() {
                remove_library_entry(&mut state.entries, id);
            }
            id
        };
        if removed_id.is_none() {
            return;
        }
        if let Some(app) = self.app() {
            let open_id = {
                let state = self.shared.lock().unwrap();
                state.modal_item.as_ref().map(|m| m.id.clone())
            };
            if open_id == removed_id {
                app.set_in_library(false);
            }
        }
        self.persist_library();
        self.apply_library_to_ui();
    }

    /// Open a saved library item: reuse the detail page and the normal
    /// movie -> streams / series -> episodes flow. The `index` is into the
    /// *filtered* grid view (the `library` model), so it is resolved against
    /// `current_library_view`.
    pub(super) fn open_library_item(&self, index: usize) {
        let entry = self.current_library_view().get(index).cloned();
        let Some(e) = entry else { return };
        let app = match self.app() {
            Some(a) => a,
            None => return,
        };

        {
            let mut state = self.shared.lock().unwrap();
            state.modal_item = Some(ModalItem {
                open_token: Arc::new(()),
                pending_watch_now: None,
                episodes_loading: false,
                id: e.id.clone(),
                type_: e.type_.clone(),
                request_id: e.id.clone(),
                videos: Vec::new(),
                season_backdrops: HashMap::new(),
                seasons: Vec::new(),
                season_index: 0,
                episode_page: 0,
                name: e.name.clone(),
                year: e.year.clone(),
                poster_url: e.poster_url.clone(),
                // Persisted since the backdrop-URL fix; older entries carry
                // "" and are upgraded by the meta fetch below. Same for
                // genres/description (persisted since the header-text fix).
                background_url: e.background_url.clone(),
                description: e.description.clone(),
                genres: e.genres.clone(),
            });
            state.streams.clear();
        }

        // Prefetched header fills gaps for entries saved before the
        // header-text fix (or saved from a thin catalog preview): the
        // merged values below paint on the first frame, and are written
        // back so restarts stay instant without another meta fetch.
        self.fill_modal_gaps_from_header_cache(&e.id);
        let (paint_background, paint_description, paint_genres, paint_year) = {
            let state = self.shared.lock().unwrap();
            match state.modal_item.as_ref() {
                Some(m) => (
                    m.background_url.clone(),
                    m.description.clone(),
                    m.genres.clone(),
                    m.year.clone(),
                ),
                None => return,
            }
        };
        // Write back anything the header cache filled, so the next open
        // (and restart) reads it straight from the entry.
        if !paint_background.is_empty() && e.background_url.is_empty() {
            self.persist_backdrop_for(&e.id, &paint_background);
        }
        self.persist_header_for(&e.id, &paint_genres, &paint_description, &paint_year, false);

        app.set_selected_title(SharedString::from(&e.name));
        app.set_selected_year(SharedString::from(&paint_year));
        app.set_selected_index(-1);
        self.clear_streams();
        app.set_modal_visible(true);
        app.set_modal_episodes(false);
        app.set_detail_deep_stream(false);
        app.set_episode_context(SharedString::default());
        app.set_season_names(Rc::new(VecModel::<SharedString>::from(vec![])).into());
        app.set_season_combo_idx(-1);
        app.set_season_cards(Rc::new(VecModel::<SeasonCard>::from(vec![])).into());
        app.set_episode_rows(Rc::new(VecModel::<EpisodeRow>::from(vec![])).into());
        app.set_selected_poster(Image::default());
        // Sync fast path (mirrors episode thumbnails): the persisted URL +
        // decoded LRU paint instantly on reentry, including after restart
        // (pixels stay file-cached). Misses keep the placeholder and fall
        // back to the async load below.
        if let Some(pixels) = backdrop_pixels_cached(&paint_background) {
            app.set_selected_backdrop(Image::from_rgba8(pixels));
        } else {
            app.set_selected_backdrop(Image::default());
            if !paint_background.is_empty() {
                self.load_detail_backdrop(paint_background.clone(), e.id.clone());
            }
        }
        // Header text paints synchronously from the persisted snapshot (or
        // prefetched header cache) so pills + synopsis don't wait for (or
        // flash in after) the meta fetch. Older entries with empty snapshots
        // keep the previous behaviour: blank now, upgraded when meta answers.
        if paint_description.is_empty() {
            app.set_selected_description(SharedString::default());
        } else {
            app.set_selected_description(SharedString::from(&paint_description));
        }
        app.set_selected_genre_list(
            Rc::new(VecModel::from(
                paint_genres
                    .iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        app.set_detail_tab(0);
        app.set_episode_filter(SharedString::default());
        app.set_in_library(true);
        self.sync_modal_category_flags();

        // Fast path: the library grid may already hold this item's decoded
        // poster (the user is looking at it right now). Reuse it so the
        // detail page shows the image instantly instead of a blank flash
        // while the background decode finishes.
        let mut poster_shown = false;
        if !e.poster_url.is_empty() {
            let model = app.get_library();
            let count = model.row_count();
            for i in 0..count {
                let Some(card) = model.row_data(i) else {
                    continue;
                };
                let card: MediaCard = card;
                if card.id == e.id.as_str() && card.is_loaded {
                    app.set_selected_poster(card.poster);
                    poster_shown = true;
                    break;
                }
            }
        }

        // Fallback: poster loads off the UI thread (cached bytes decode
        // fast, misses are downloaded); opening the page stays instant.
        if !poster_shown && !e.poster_url.is_empty() {
            self.load_detail_poster(e.poster_url.clone(), e.id.clone());
        }

        // Grid return target: backing out re-focuses the opened card (the
        // lifted kb props survive the grid page's recreation). `index` is
        // into the filtered view, which persists via the filter property.
        app.set_library_kb_zone(2);
        app.set_library_kb_idx(index as i32);
        // Same-entry reopen restores tab/filter/focus (else resets).
        self.restore_detail_snapshot(&e.id);

        if e.type_ == "movie" {
            self.start_stream_search(e.id);
        } else {
            self.prepare_episodes(e.id, e.type_);
        }
    }

    pub(super) fn show_library_page(&self) {
        if let Some(app) = self.app() {
            app.set_show_home(false);
            app.set_show_library(true);
            app.set_show_settings(false);
        }
        // Prefetch episode metadata for saved series/anime so opening them
        // from My Library is instant. Unlike the Discover prefetch, this is
        // not gated by the "Prefetch episode metadata" setting: My Library is
        // the user's own curated list, so it should always be ready.
        self.prefetch_library_meta();
    }

    pub(super) fn add_category_to_ui(&self, name: &str) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        // Built-in automatic filters are reserved: a user category with
        // the same name would hijack the library filter.
        if BUILTIN_FILTERS.contains(&name.as_str()) {
            return;
        }
        {
            let mut state = self.shared.lock().unwrap();
            add_category(&mut state.cache_settings.categories, &name);
            write_settings(&state.cache_settings);
        }
        self.apply_category_rows();
    }

    pub(super) fn remove_category_from_ui(&self, index: usize) {
        let name = {
            let state = self.shared.lock().unwrap();
            state.cache_settings.categories.get(index).cloned()
        };
        let Some(name) = name else { return };
        {
            let mut state = self.shared.lock().unwrap();
            state.cache_settings.categories.retain(|c| *c != name);
            for e in state.entries.iter_mut() {
                e.categories.retain(|c| c != &name);
            }
            write_settings(&state.cache_settings);
            write_persisted_library(&state.entries);
        }
        // If the deleted category was the active filter, reset to "All".
        if let Some(app) = self.app()
            && app.get_library_filter_category().as_str() == name
        {
            app.set_library_filter_category(SharedString::default());
        }
        self.apply_category_rows();
        self.apply_library_to_ui();
    }

    pub(super) fn toggle_entry_category(&self, cat_name: &str) {
        let (entry_id, current_cats) = {
            let state = self.shared.lock().unwrap();
            match state.modal_item.as_ref() {
                Some(m) => {
                    let entry = state.entries.iter().find(|e| e.id == m.id);
                    (
                        m.id.clone(),
                        entry.map(|e| e.categories.clone()).unwrap_or_default(),
                    )
                }
                None => return,
            }
        };
        let mut new_cats = current_cats;
        if new_cats.iter().any(|c| c == cat_name) {
            new_cats.retain(|c| c != cat_name);
        } else {
            new_cats.push(cat_name.to_string());
        }
        {
            let mut state = self.shared.lock().unwrap();
            set_entry_categories(&mut state.entries, &entry_id, new_cats);
            write_persisted_library(&state.entries);
        }
        self.apply_library_to_ui();
        // Refresh modal_category_flags in the detail modal.
        self.sync_modal_category_flags();
    }

    pub(super) fn sync_modal_category_flags(&self) {
        let (entry_id, all_cats) = {
            let state = self.shared.lock().unwrap();
            match state.modal_item.as_ref() {
                Some(m) => (m.id.clone(), state.cache_settings.categories.clone()),
                None => return,
            }
        };
        let entry_cats = {
            let state = self.shared.lock().unwrap();
            state
                .entries
                .iter()
                .find(|e| e.id == entry_id)
                .map(|e| e.categories.clone())
                .unwrap_or_default()
        };
        let flags: Vec<bool> = all_cats
            .iter()
            .map(|c| entry_cats.iter().any(|ec| ec == c))
            .collect();
        if let Some(app) = self.app() {
            let count = flags.iter().filter(|f| **f).count() as i32;
            app.set_selected_category_count(count);
            app.set_modal_category_flags(Rc::new(VecModel::from(flags)).into());
        }
    }

    pub(super) fn filter_library(&self, category: &str) {
        if let Some(app) = self.app() {
            app.set_library_filter_category(SharedString::from(category));
        }
        self.sync_library_category_index();
        self.apply_library_to_ui();
    }

    /// Index of the active library category filter within the category
    /// names (-1 = All). Keeps the filter dropdown's selection in sync when
    /// the filter is set from the "All" chip, keyboard nav, or category
    /// add/remove rather than the dropdown itself.
    pub(super) fn sync_library_category_index(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let filter = app.get_library_filter_category().to_string();
        let cats = self
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .categories
            .clone();
        // Dropdown order: built-ins first (see apply_category_rows),
        // then user categories. -1 = All.
        let idx = if filter.is_empty() {
            -1
        } else if let Some(i) = BUILTIN_FILTERS.iter().position(|b| *b == filter) {
            i as i32
        } else {
            cats.iter()
                .position(|c| c == &filter)
                .map(|i| (i + BUILTIN_FILTERS.len()) as i32)
                .unwrap_or(-1)
        };
        app.set_library_category_index(idx);
    }

    /// Sync the category list from settings to the UI. Built-in automatic
    /// filters lead the dropdown, user categories follow.
    pub(super) fn apply_category_rows(&self) {
        let cats = {
            let state = self.shared.lock().unwrap();
            state.cache_settings.categories.clone()
        };
        let rows: Vec<CategoryRow> = cats
            .iter()
            .map(|c| CategoryRow {
                name: SharedString::from(c),
            })
            .collect();
        // `category_names` holds the values the filter is keyed on (the
        // English identifiers also used by `BUILTIN_FILTERS`/`auto_bucket`);
        // `category_labels` is what the dropdown shows — the same list with
        // the automatic buckets translated. Picking an entry still sends back
        // the value, so a localized label can never leak into the data.
        let names: Vec<SharedString> = BUILTIN_FILTERS
            .iter()
            .map(|s| SharedString::from(*s))
            .chain(cats.iter().map(SharedString::from))
            .collect();
        let labels: Vec<SharedString> = BUILTIN_FILTERS
            .iter()
            .map(|s| SharedString::from(text::tr(s)))
            .chain(cats.iter().map(SharedString::from))
            .collect();
        if let Some(app) = self.app() {
            app.set_category_rows(Rc::new(VecModel::from(rows)).into());
            app.set_library_category_names(Rc::new(VecModel::from(names)).into());
            app.set_library_category_labels(Rc::new(VecModel::from(labels)).into());
        }
        self.sync_library_category_index();
    }
}

/// Add/overwrite an entry in place, keeping entries unique by `id` and
/// preserving insertion order. Returns true when the entry was newly added.
pub(crate) fn upsert_library(entries: &mut Vec<LibraryEntry>, entry: LibraryEntry) -> bool {
    if let Some(existing) = entries.iter_mut().find(|e| e.id == entry.id) {
        *existing = entry;
        false
    } else {
        entries.push(entry);
        true
    }
}
/// Remove an entry by `id`. Returns true when something was removed.
pub(crate) fn remove_library_entry(entries: &mut Vec<LibraryEntry>, id: &str) -> bool {
    let before = entries.len();
    entries.retain(|e| e.id != id);
    entries.len() != before
}
/// Persisted library entries (empty when absent/unreadable).
pub(crate) fn read_persisted_library() -> Vec<LibraryEntry> {
    read_json::<Vec<LibraryEntry>>("library").unwrap_or_default()
}
/// Snapshot library entries into the KV store.
pub(crate) fn write_persisted_library(entries: &[LibraryEntry]) {
    write_json("library", entries);
    // Mirror into the sync store (no-op when sync is off or this write is a
    // remote apply).
    if !applying() {
        notify_library(entries);
    }
}
/// Add a new category name to settings (deduplicates, preserves order).
pub(crate) fn add_category(categories: &mut Vec<String>, name: &str) {
    if !categories.iter().any(|c| c == name) {
        categories.push(name.to_string());
    }
}
/// Remove a category name from settings and strip it from all library entries.
#[allow(dead_code)]
pub(crate) fn remove_category(
    categories: &mut Vec<String>,
    entries: &mut [LibraryEntry],
    name: &str,
) {
    categories.retain(|c| c != name);
    for e in entries.iter_mut() {
        e.categories.retain(|c| c != name);
    }
}
/// Rename a category everywhere (settings + all library entries).
#[allow(dead_code)]
pub(crate) fn rename_category(
    categories: &mut [String],
    entries: &mut [LibraryEntry],
    old: &str,
    new: &str,
) {
    if let Some(c) = categories.iter_mut().find(|c| **c == old) {
        *c = new.to_string();
    }
    for e in entries.iter_mut() {
        if let Some(c) = e.categories.iter_mut().find(|c| **c == old) {
            *c = new.to_string();
        }
    }
}
/// Set the categories for a single library entry (by id).
pub(crate) fn set_entry_categories(entries: &mut [LibraryEntry], id: &str, cats: Vec<String>) {
    if let Some(e) = entries.iter_mut().find(|e| e.id == id) {
        e.categories = cats;
    }
}
/// Fold fresh header text into a library entry. Returns true when anything
/// changed. With `overwrite`, fresh non-empty values replace stored text
/// (unfinished-show refresh saves updated data); otherwise only empty slots
/// are filled (backfill without addon churn). Fresh empty values never blank
/// stored text in either mode.
pub(crate) fn merge_library_header_text(
    entry: &mut LibraryEntry,
    genres: &[String],
    description: &str,
    year: &str,
    overwrite: bool,
) -> bool {
    let mut touched = false;
    if !genres.is_empty()
        && (overwrite || entry.genres.is_empty())
        && entry.genres.as_slice() != genres
    {
        entry.genres = genres.to_vec();
        touched = true;
    }
    if !description.is_empty()
        && (overwrite || entry.description.is_empty())
        && entry.description != description
    {
        entry.description = description.to_string();
        touched = true;
    }
    if !year.is_empty() && (overwrite || entry.year.is_empty()) && entry.year != year {
        entry.year = year.to_string();
        touched = true;
    }
    touched
}

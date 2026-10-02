//! Detail modal: metadata, seasons/episodes, stream list.
use super::*;

const STREAM_PAGE_SIZE: usize = 25;

/// Number of pages a season of `total` filtered episodes needs (always ≥ 1).
pub(crate) fn page_count(total: usize) -> usize {
    total.div_ceil(EPISODE_PAGE_SIZE).max(1)
}

impl Bridge {
    pub(super) fn item_selected(&self, index: usize) {
        let searching = self.app().is_some_and(|app| app.get_discover_search_open());
        let (preview, generation) = {
            let state = self.shared.lock().unwrap();
            let preview = if searching {
                state.search_previews.get(index).cloned()
            } else {
                state.previews.get(index).cloned()
            };
            (
                preview,
                if searching {
                    state.search_generation
                } else {
                    state.catalog_gen
                },
            )
        };
        // The (generation, index) poster fast path is native-only; Android
        // relies on the decoded LRU via load_detail_poster instead.
        #[cfg(not(feature = "desktop"))]
        let _ = generation;
        let preview = match preview {
            Some(p) => p,
            None => return,
        };

        self.open_preview(preview, generation, searching, Some(index), false);
    }

    /// Open any catalog preview in the shared detail flow. Home's featured
    /// showcase supplies its own preview without borrowing or mutating the
    /// Discover grid's current catalog model.
    pub(super) fn open_preview(
        &self,
        preview: MetaPreview,
        generation: u64,
        searching: bool,
        discover_index: Option<usize>,
        watch_now: bool,
    ) {
        let app = match self.app() {
            Some(a) => a,
            None => return,
        };

        tracking::remember_source(&preview);
        if let Some(app) = self.app() {
            app.set_tracking_open(false);
        }
        {
            let mut state = self.shared.lock().unwrap();
            state.modal_item = Some(ModalItem {
                open_token: Arc::new(()),
                pending_watch_now: (watch_now && preview.type_ != "movie")
                    .then_some(WatchNowOrigin::Featured),
                episodes_loading: false,
                id: preview.id.clone(),
                type_: if preview.type_.is_empty() {
                    state
                        .type_defs
                        .get(state.chosen_type)
                        .map(|t| t.type_.clone())
                        .unwrap_or_default()
                } else {
                    preview.type_.clone()
                },
                request_id: preview.id.clone(),
                videos: Vec::new(),
                seasons: Vec::new(),
                season_index: 0,
                episode_page: 0,
                name: preview.title(),
                year: preview.year_str().unwrap_or_default(),
                poster_url: preview.poster.clone().unwrap_or_default(),
                background_url: preview.background.clone().unwrap_or_default(),
                description: preview.description.clone().unwrap_or_default(),
                genres: preview.genres.clone(),
            });
            state.streams.clear();
        }

        // Prefetched header fills catalog gaps (some catalogs omit
        // description/genres): modal gaps first, then paint from the modal
        // so cached text shows on the first frame. The meta fetch still
        // upgrades anything still missing when it answers.
        self.fill_modal_gaps_from_header_cache(&preview.id);
        let (paint_description, paint_genres, paint_year) = {
            let state = self.shared.lock().unwrap();
            match state.modal_item.as_ref() {
                Some(m) => (m.description.clone(), m.genres.clone(), m.year.clone()),
                None => return,
            }
        };

        app.set_selected_title(SharedString::from(preview.title()));
        app.set_selected_year(SharedString::from(&paint_year));
        app.set_selected_index(discover_index.map(|i| i as i32).unwrap_or(-1));
        if let Some(index) = discover_index {
            // Grid return target: backing out re-focuses the opened card (the
            // lifted kb props survive the grid page's recreation).
            app.set_discover_kb_zone(3);
            app.set_discover_kb_idx(index as i32);
        }
        self.clear_streams();
        app.set_modal_visible(true);
        // Reset any leftover episode-picker state from a previous item.
        app.set_modal_episodes(false);
        app.set_detail_deep_stream(false);
        app.set_episode_context(SharedString::default());
        app.set_season_names(Rc::new(VecModel::<SharedString>::from(vec![])).into());
        app.set_season_combo_idx(-1);
        app.set_season_cards(Rc::new(VecModel::<SeasonCard>::from(vec![])).into());
        app.set_episode_rows(Rc::new(VecModel::<EpisodeRow>::from(vec![])).into());
        // Detail header (banner layout): description/genres from the catalog
        // preview (or prefetched header cache when the preview omits them);
        // the meta fetch upgrades anything still missing when it answers.
        app.set_selected_description(SharedString::from(&paint_description));
        app.set_selected_genre_list(
            Rc::new(VecModel::from(
                paint_genres
                    .iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        // Backdrop is painted below via the sync cache fast path (cached
        // reentry paints instantly; misses blank + async-load).
        app.set_detail_tab(0);
        app.set_episode_filter(SharedString::default());

        // Reset the detail poster first so an item without one never keeps
        // showing the previously selected poster; fill it from the cache if
        // the grid download already finished (the poster worker also updates
        // it once a not-yet-cached download completes). On Android the decoded
        // LRU (filled by the grid fetch) plays the same role via
        // load_detail_poster's fast path.
        app.set_selected_poster(Image::default());
        // The player's loading screen falls back to the show poster, so it
        // must be reset alongside (a stale episode thumb from the previous
        // item would otherwise survive).
        app.set_player_poster(Image::default());
        #[cfg(feature = "desktop")]
        if !searching
            && let Some(index) = discover_index
            && let Some(buffer) = self.poster_cache.lock().unwrap().get(&(generation, index))
        {
            app.set_selected_poster(Image::from_rgba8(buffer.clone()));
        }

        app.set_in_library(self.library_contains(&preview.id));
        if self.library_contains(&preview.id) {
            self.sync_modal_category_flags();
        }

        let (item_id, modal_type, backdrop_url, header) = {
            let state = self.shared.lock().unwrap();
            let m = state.modal_item.as_ref().unwrap();
            (
                m.id.clone(),
                m.type_.clone(),
                m.background_url.clone(),
                (m.genres.clone(), m.description.clone(), m.year.clone()),
            )
        };
        // Sync fast path (mirrors episode thumbnails): paint the cached
        // backdrop immediately on reentry instead of flashing through the
        // placeholder + an async round-trip. Misses fall back to the async
        // load below (which populates the cache for next time).
        if let Some(pixels) = backdrop_pixels_cached(&backdrop_url) {
            app.set_selected_backdrop(Image::from_rgba8(pixels));
        } else {
            app.set_selected_backdrop(Image::default());
            if !backdrop_url.is_empty() {
                self.load_detail_backdrop(backdrop_url.clone(), item_id.clone());
            }
        }
        // Backfill the persisted library URL so library reopens (and
        // restarts) can start the image load without a meta fetch.
        self.persist_backdrop_for(&item_id, &backdrop_url);
        // Backfill persisted header text for the same reason.
        self.persist_header_for(&item_id, &header.0, &header.1, &header.2, false);
        // Same-entry reopen restores tab/filter/focus (else resets).
        self.restore_detail_snapshot(&item_id);

        if modal_type == "movie" {
            self.cancel_watch_now();
            // Movies keep the direct behaviour: fetch streams right away.
            self.start_stream_search(item_id);
        } else {
            // Series-like: list seasons/episodes first and only fetch
            // streams once an episode is picked.
            self.prepare_episodes(item_id, modal_type);
        }
    }

    /// Fetch and display streams for `request_id` (a movie id, or the
    /// `<item>:<season>:<episode>` id of a picked episode) from every
    /// installed addon that provides streams for the modal item's type.
    pub(super) fn start_stream_search(&self, request_id: String) {
        let modal_type = {
            let state = self.shared.lock().unwrap();
            state
                .modal_item
                .as_ref()
                .map(|m| m.type_.clone())
                .unwrap_or_default()
        };
        if modal_type.is_empty() {
            return;
        }

        // Endpoint URLs built on the main thread (pure URL math); the rows
        // are then fetched addon-by-addon via fetch-continuations. Each URL
        // carries its addon label so results can be grouped into pills.
        let stream_urls: Vec<(String, String)> = {
            let state = self.shared.lock().unwrap();
            let lookup = state
                .modal_item
                .as_ref()
                .and_then(|modal| source_stream_lookup(modal, &request_id));
            let stream_ids = state
                .modal_item
                .as_ref()
                .map(|modal| episode_stream_ids(modal, &request_id))
                .unwrap_or_default();
            state
                .installed
                .iter()
                .filter_map(|a| {
                    stream_endpoint(a, &modal_type, &request_id, lookup.as_ref(), &stream_ids)
                        .map(|url| (a.label.clone(), url))
                })
                .collect()
        };

        let requested: Vec<String> = stream_urls.iter().map(|(l, _)| l.clone()).collect();
        let addon_count = requested.len();
        // Show one pill per addon being queried right away (spinner), then
        // drop each one that answers with no streams.
        let generation = {
            let mut state = self.shared.lock().unwrap();
            if let Some(m) = state.modal_item.as_mut() {
                m.request_id = request_id.clone();
            }
            state.stream_generation = state.stream_generation.wrapping_add(1);
            state.streams.clear();
            state.stream_all.clear();
            state.stream_addons = requested.clone();
            state.stream_pending = requested.clone();
            state.stream_filter = 0;
            state.stream_generation
        };
        if let Some(app) = self.app() {
            app.set_stream_addons(
                Rc::new(VecModel::from(
                    requested.iter().map(SharedString::from).collect::<Vec<_>>(),
                ))
                .into(),
            );
            app.set_stream_loading(Rc::new(VecModel::from(vec![true; requested.len()])).into());
            app.set_streams_searching(!requested.is_empty());
            app.set_stream_filter(0);
        }
        // Show already-downloaded rows for this request immediately, before
        // the first addon answers. Addon streams keep streaming in per addon
        // (see `stream_addon_loaded`), pinned below the downloads.
        self.apply_stream_filter();
        // `apply_stream_filter` picks the generic hint; restore the specific
        // "no addon provides streams" message when there is nothing to query.
        if addon_count == 0 {
            self.set_stream_hint(Some(StreamHint::Fixed(
                "No installed addon provides streams for this type.",
            )));
        }

        // Query every addon in parallel; each answer is pushed to the UI the
        // moment it arrives, so results appear addon-by-addon instead of
        // after the slowest one. An addon that fails or returns nothing is
        // reported with an empty row list so its pill is removed.
        for (addon, url) in stream_urls {
            let bridge = self.clone();
            let request_id = request_id.clone();
            net::fetch_bytes(url, move |result| {
                let new_rows: Vec<StreamUi> = result
                    .ok()
                    .and_then(|bytes| Addon::parse_streams(&bytes).ok())
                    .map(|streams| {
                        streams
                            .into_iter()
                            .take(80)
                            .map(|s| {
                                let id = format!(
                                    "stream-{:016x}",
                                    bridge.stream_seq.fetch_add(1, Ordering::Relaxed)
                                );
                                StreamUi {
                                    id,
                                    // Rows render multi-line; see
                                    // `stream_display` for how the label +
                                    // detail lines are chosen.
                                    display: stream_display(&s),
                                    source: stream_source(&s),
                                    addon: addon.clone(),
                                    download: None,
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let bridge2 = bridge.clone();
                let addon2 = addon.clone();
                let request_id2 = request_id.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    bridge2.stream_addon_loaded(request_id2, generation, addon2, new_rows);
                });
            });
        }
    }

    /// Series-like item: try to list its episodes (seasons) from the first
    /// installed addon with a `meta` resource for this type. Falls back to
    /// the plain stream search when nobody can list episodes.
    pub(super) fn prepare_episodes(&self, id: String, modal_type: String) {
        let open_token = {
            let mut state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_mut().filter(|m| m.id == id) else {
                return;
            };
            modal.episodes_loading = true;
            modal.open_token.clone()
        };
        // Fast path: if this item's episode list was fetched before, show the
        // cached version instantly while the background refresh below runs.
        let had_cache = if let Some(cached) = read_episodes_cache_for(&modal_type, &id) {
            let still_open = {
                let state = self.shared.lock().unwrap();
                state
                    .modal_item
                    .as_ref()
                    .map(|m| m.id == id)
                    .unwrap_or(false)
            };
            if still_open {
                self.show_episode_picker(id.clone(), cached);
            }
            true
        } else {
            false
        };

        let candidates: Vec<Installed> = {
            let state = self.shared.lock().unwrap();
            state
                .installed
                .iter()
                .filter(|a| a.enabled && a.manifest.accepts("meta", &modal_type, &id))
                .cloned()
                .collect()
        };

        // Keep the streams area showing a neutral hint until we know whether
        // the addon can list episodes; the picker replaces it if it can.
        // (Skipped when a cached picker is already visible.)
        if !had_cache {
            let hint = if candidates.is_empty() {
                StreamHint::Fixed("No installed addon provides details for this type.")
            } else {
                StreamHint::LoadingEpisodes(candidates.len())
            };
            self.set_stream_hint(Some(hint));
        }

        // Background refresh: fetch fresh episode metadata and remember it on
        // disk so the next visit renders instantly. `refresh` tells the apply
        // step whether a cached picker is already visible (then the fresh data
        // must not yank the user out of a deeper view) or this is the very
        // first population (then the picker must be shown unconditionally).
        let refresh = had_cache;
        let bridge = self.clone();
        // Meta endpoint URLs built on the main thread; addons are tried in
        // order via fetch-continuations until one lists episodes.
        let meta_urls: Vec<String> = candidates
            .iter()
            .filter_map(|c| Addon::new(&c.url).ok())
            .map(|a| a.meta_url(&modal_type, &id))
            .collect();

        fn run(
            bridge: Bridge,
            modal_type: String,
            id: String,
            refresh: bool,
            open_token: Arc<()>,
            mut urls: Vec<String>,
            mut found: Option<MetaItem>,
        ) {
            if let Some(item) = found {
                let videos: Vec<Video> = item
                    .videos
                    .iter()
                    .filter(|v| v.season.is_some())
                    .cloned()
                    .collect();
                write_episodes_cache_for(&modal_type, &id, &videos);
                let bridge2 = bridge.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    bridge2.apply_episode_meta(id, Some(item), refresh, open_token);
                });
                return;
            }
            let Some(url) = (!urls.is_empty()).then(|| urls.remove(0)) else {
                let bridge2 = bridge.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    bridge2.apply_episode_meta(id, None, refresh, open_token);
                });
                return;
            };
            let bridge2 = bridge.clone();
            net::fetch_bytes(url, move |result| {
                if let Ok(bytes) = result
                    && let Ok(Some(item)) = Addon::parse_meta(&bytes)
                    && item.videos.iter().any(|v| v.season.is_some())
                {
                    found = Some(item);
                }
                run(bridge2, modal_type, id, refresh, open_token, urls, found);
            });
        }

        run(bridge, modal_type, id, refresh, open_token, meta_urls, None);
    }

    /// Fill empty `ModalItem` header slots (background/description/genres/
    /// year) from the prefetched header cache. Returns true when anything
    /// was filled. Callers repaint text from the modal afterwards; the
    /// backdrop fast path picks up a filled URL via its normal read.
    pub(super) fn fill_modal_gaps_from_header_cache(&self, id: &str) -> bool {
        let cached = {
            let state = self.shared.lock().unwrap();
            let m = match state.modal_item.as_ref() {
                Some(m) if m.id == id => m,
                _ => return false,
            };
            if !m.background_url.is_empty()
                && !m.description.is_empty()
                && !m.genres.is_empty()
                && !m.year.is_empty()
            {
                return false;
            }
            read_meta_header_for(&m.type_, &m.id)
        };
        let Some(cached) = cached else {
            return false;
        };
        let mut state = self.shared.lock().unwrap();
        let m = match state.modal_item.as_mut() {
            Some(m) if m.id == id => m,
            _ => return false,
        };
        let mut touched = false;
        if m.background_url.is_empty() && !cached.background_url.is_empty() {
            m.background_url = cached.background_url.clone();
            touched = true;
        }
        if m.description.is_empty() && !cached.description.is_empty() {
            m.description = cached.description.clone();
            touched = true;
        }
        if m.genres.is_empty() && !cached.genres.is_empty() {
            m.genres.clone_from(&cached.genres);
            touched = true;
        }
        if m.year.is_empty() && !cached.year.is_empty() {
            m.year.clone_from(&cached.year);
            touched = true;
        }
        touched
    }

    /// Upgrade the detail header (backdrop/poster/description/genres) from a
    /// full meta response. By default only fills gaps: catalog-preview values
    /// win when the meta response omits a field. With `overwrite` (used for
    /// unfinished shows on entry, whose text may have changed since it was
    /// cached), fresh non-empty text fields replace the shown values.
    /// `refresh_images` runs the poster/backdrop refresh check on any
    /// successful meta fetch regardless of finished status: fresh art
    /// replaces the shown images (reloaded even when their URLs are
    /// unchanged, so same-URL updates are picked up too).
    pub(super) fn upgrade_detail_meta(
        &self,
        id: &str,
        item: &MetaItem,
        overwrite: bool,
        refresh_images: bool,
    ) {
        let (backdrop_to_load, poster_to_load, header, header_cache, modal_type) = {
            let mut state = self.shared.lock().unwrap();
            let m = match state.modal_item.as_mut() {
                Some(m) if m.id == id => m,
                _ => return,
            };
            let mut backdrop_to_load: Option<String> = None;
            let mut poster_to_load: Option<String> = None;
            if let Some(bg) = &item.preview.background
                && !bg.is_empty()
                && (m.background_url.is_empty() || overwrite)
            {
                m.background_url = bg.clone();
                backdrop_to_load = Some(bg.clone());
            }
            if let Some(desc) = &item.preview.description
                && !desc.is_empty()
                && (m.description.is_empty() || overwrite)
            {
                m.description = desc.clone();
            }
            if (m.genres.is_empty() || overwrite) && !item.preview.genres.is_empty() {
                m.genres = item.preview.genres.clone();
            }
            if (m.year.is_empty() || overwrite)
                && let Some(year) = item.preview.year_str()
            {
                m.year = year;
            }
            // Poster upgrades only ever overwrite: the entry flow paints the
            // catalog/library poster instantly, and only a fresh meta
            // response justifies replacing it. The art refresh itself is
            // not finished-gated (see `refresh_images`): posters change
            // upstream for ended shows too.
            if refresh_images {
                if let Some(poster) = &item.preview.poster
                    && !poster.is_empty()
                {
                    m.poster_url = poster.clone();
                }
                // Reload even when the URL is unchanged: the bytes behind it
                // may have changed since they were cached (the refresh check
                // below re-downloads and swaps only on real pixel changes).
                if !m.poster_url.is_empty() {
                    poster_to_load = Some(m.poster_url.clone());
                }
                if backdrop_to_load.is_none() && !m.background_url.is_empty() {
                    backdrop_to_load = Some(m.background_url.clone());
                }
            }
            let header = (m.genres.clone(), m.description.clone(), m.year.clone());
            let header_cache = MetaHeader {
                background_url: m.background_url.clone(),
                description: m.description.clone(),
                genres: m.genres.clone(),
                year: m.year.clone(),
            };
            let modal_type = m.type_.clone();
            (
                backdrop_to_load,
                poster_to_load,
                header,
                header_cache,
                modal_type,
            )
        };
        // Backfill persisted text so later library reopens paint instantly.
        // Overwrites save the refreshed text for unfinished shows.
        self.persist_header_for(id, &header.0, &header.1, &header.2, overwrite);
        // Feed the header cache too, so prefetches can skip this item and
        // other opens (e.g. Discover reentry with a thin catalog preview)
        // fill gaps synchronously. Overwrites replace the cached header
        // outright; the default path only fills empty slots.
        if overwrite {
            write_meta_header_for(&modal_type, id, &header_cache);
        } else {
            merge_meta_header_for(&modal_type, id, &header_cache);
        }
        if let Some(app) = self.app() {
            let state = self.shared.lock().unwrap();
            if let Some(m) = state.modal_item.as_ref()
                && m.id == id
            {
                app.set_selected_description(SharedString::from(m.description.clone()));
                app.set_selected_genre_list(
                    Rc::new(VecModel::from(
                        m.genres.iter().map(SharedString::from).collect::<Vec<_>>(),
                    ))
                    .into(),
                );
                if !m.year.is_empty() {
                    app.set_selected_year(SharedString::from(m.year.clone()));
                }
            }
        }
        if let Some(url) = poster_to_load {
            // Save the updated poster URL so library reopens (and restarts)
            // paint the grid + detail header from the entry without waiting.
            self.persist_poster_for(id, &url);
            // Refresh check, not an evict-then-load: the shown poster stays
            // painted while fresh bytes download, repainting only on a real
            // pixel change — no placeholder flash for identical art.
            self.refresh_detail_poster(url, id.to_string());
        }
        if let Some(url) = backdrop_to_load {
            // Persist for library reopens, then paint instantly when the
            // pixels are already cached (common on reentry).
            self.persist_backdrop_for(id, &url);
            if refresh_images {
                // Refresh check (same no-flash contract as the poster).
                self.refresh_detail_backdrop(url, id.to_string());
                return;
            }
            if let Some(app) = self.app()
                && let Some(pixels) = backdrop_pixels_cached(&url)
            {
                let still_current = {
                    let state = self.shared.lock().unwrap();
                    state
                        .modal_item
                        .as_ref()
                        .map(|m| m.id == id)
                        .unwrap_or(false)
                };
                if still_current {
                    app.set_selected_backdrop(Image::from_rgba8(pixels));
                    return;
                }
            }
            self.load_detail_backdrop(url, id.to_string());
        }
    }

    /// Main thread: apply fresh episode metadata. The list is swapped in
    /// while the user is still on the unselected top-level episode list; a
    /// deeper view (a season chosen or streams shown) keeps its position but
    /// still receives the fresh videos in place when the show is unfinished.
    /// When no addon can provide episodes, fall back to plain streams unless
    /// a cached list is already being shown.
    pub(super) fn apply_episode_meta(
        &self,
        id: String,
        found: Option<MetaItem>,
        refresh: bool,
        open_token: Arc<()>,
    ) {
        let (still_open, modal_type) = {
            let mut state = self.shared.lock().unwrap();
            match state.modal_item.as_mut() {
                Some(m) if m.id == id && Arc::ptr_eq(&m.open_token, &open_token) => {
                    m.episodes_loading = false;
                    (true, m.type_.clone())
                }
                _ => (false, String::new()),
            }
        };
        if !still_open {
            return;
        }
        if let Some(item) = found.as_ref() {
            self.tracking_metadata(id.clone(), modal_type.clone(), &item.videos);
        }
        // Unfinished shows may have new or changed episodes and descriptions
        // since the cache was written: their header text is overwritten (not
        // gap-filled) and fresh videos swap in even past the top-level list.
        // Finished shows keep the cheap cached behavior for text — but art
        // is rechecked on every successful fetch regardless of status (see
        // `refresh_images`): posters change upstream for ended shows too.
        let unfinished = found
            .as_ref()
            .is_some_and(|item| !Self::series_finished(item.status.as_deref()));
        let refresh_images = found.is_some();
        let status = found.as_ref().and_then(|item| item.status.clone());
        // Header upgrade applies regardless of the picker refresh guards.
        if let Some(item) = found.as_ref() {
            self.upgrade_detail_meta(&id, item, unfinished, refresh_images);
            // Fresh stream aliases must reach an already-selected cached
            // episode too, even when the picker itself stays in place.
            let retry = {
                let mut state = self.shared.lock().unwrap();
                state.modal_item.as_mut().and_then(|modal| {
                    let request_id = modal.request_id.clone();
                    let before = episode_stream_ids(modal, &request_id);
                    merge_episode_stream_ids(&mut modal.videos, &item.videos);
                    (before != episode_stream_ids(modal, &request_id)).then_some(request_id)
                })
            };
            if let Some(request_id) = retry
                && self.app().is_some_and(|app| {
                    app.get_modal_visible()
                        && !app.get_player_open()
                        && !app.get_episode_context().is_empty()
                })
            {
                self.start_stream_search(request_id);
            }
        }
        match found {
            Some(item) => {
                let videos: Vec<Video> = item
                    .videos
                    .into_iter()
                    .filter(|v| v.season.is_some())
                    .collect();
                if !refresh {
                    // First population (nothing was cached): the picker was
                    // never shown yet, so always show it now.
                    eprintln!(
                        "nova: detail refresh {modal_type}/{id}: status={status:?}, first population, {} videos",
                        videos.len()
                    );
                    self.show_episode_picker(id, videos);
                    if unfinished {
                        self.queue_episode_thumbnail_verify();
                    }
                    // Fresh episode list: library badges/checks and Home may
                    // depend on it (e.g. newly-appeared unaired episodes).
                    self.refresh_library_progress_ui();
                    return;
                }
                let Some(app) = self.app() else {
                    return;
                };
                // Refreshing behind an already-visible cached picker: only
                // swap in the fresh data while the user is still on the
                // top-level list of this item (nothing selected, no streams).
                let on_top_level = app.get_modal_episodes()
                    && app.get_episode_context().is_empty()
                    && app.get_streams().row_count() == 0;
                let (untouched, awaiting_watch_now) = {
                    let state = self.shared.lock().unwrap();
                    state
                        .modal_item
                        .as_ref()
                        .filter(|m| m.id == id)
                        .map(|m| (m.season_index == 0, m.pending_watch_now.is_some()))
                        .unwrap_or((false, false))
                };
                // A pending explicit request also needs fresh metadata after
                // an empty/stale cache, even if no picker could be displayed.
                if (on_top_level && untouched) || awaiting_watch_now {
                    eprintln!(
                        "nova: detail refresh {modal_type}/{id}: status={status:?}, picker swap, {} videos",
                        videos.len()
                    );
                    self.show_episode_picker(id, videos);
                } else if unfinished {
                    // Background refresh for an unfinished show arriving past
                    // the top-level list (or onto a restored non-zero
                    // season): swap the videos in place so descriptions stay
                    // fresh without moving the user's season/tab/streams.
                    eprintln!(
                        "nova: detail refresh {modal_type}/{id}: status={status:?}, in-place update, {} videos",
                        videos.len()
                    );
                    self.update_episode_videos(id, videos);
                } else {
                    eprintln!(
                        "nova: detail refresh {modal_type}/{id}: status={status:?}, kept cached list (deeper view of a finished show)"
                    );
                }
                if unfinished {
                    // Refresh checks for the already-cached thumbnails (the
                    // queues above only fetch the missing ones): cached art
                    // stays painted while fresh bytes download, and rows
                    // repaint only on real pixel changes — no placeholder
                    // flash for identical posters.
                    self.queue_episode_thumbnail_verify();
                }
                // Same library/Home refresh as first population: the fresh
                // list may carry new (possibly unaired) episodes.
                self.refresh_library_progress_ui();
            }
            None => {
                // No addon can list episodes right now: keep showing a cached
                // list when we have one, otherwise fall back to the plain
                // streams behaviour.
                eprintln!("nova: detail refresh {modal_type}/{id}: no meta addon answered");
                let had_cache = read_episodes_cache_for(&modal_type, &id).is_some();
                if !had_cache {
                    self.start_stream_search(id);
                }
            }
        }
        self.resolve_watch_now();
    }

    /// Main thread: swap fresh episode metadata into the open modal without
    /// moving the user. The selected season is kept (clamped when the fresh
    /// list no longer contains it), season names are repainted, and rows are
    /// rebuilt in place — unlike `show_episode_picker`, the tab and stream
    /// list stay open. An already-picked episode receives its fresh caption
    /// and thumbnail too, including corrected canonical season numbering.
    /// Current-season thumbnails are re-queued by `refresh_episode_rows`.
    pub(super) fn update_episode_videos(&self, id: String, videos: Vec<Video>) {
        let seasons = ordered_seasons(&videos);
        if seasons.is_empty() {
            return; // never blank an open picker on a bad refresh
        }
        let (season_names, season_idx, selected, thumbnail_changed) = {
            let mut state = self.shared.lock().unwrap();
            let m = match state.modal_item.as_mut() {
                Some(m) if m.id == id => m,
                _ => return,
            };
            let old_thumbnail = m
                .videos
                .iter()
                .find(|video| video.id == m.request_id)
                .and_then(|video| video.thumbnail.clone());
            m.videos = videos;
            m.seasons = seasons;
            m.season_index = m.season_index.min(m.seasons.len() - 1);
            let idx = m.season_index;
            let names = m
                .seasons
                .iter()
                .map(|&s| SharedString::from(season_label(s)))
                .collect::<Vec<_>>();
            let selected = m
                .videos
                .iter()
                .find(|video| video.id == m.request_id)
                .cloned();
            let thumbnail_changed = selected.as_ref().and_then(|video| video.thumbnail.as_ref())
                != old_thumbnail.as_ref();
            (names, idx, selected, thumbnail_changed)
        };
        if let Some(app) = self.app() {
            app.set_season_names(Rc::new(VecModel::from(season_names)).into());
            app.set_season_combo_idx(season_idx as i32);
            if !app.get_episode_context().is_empty()
                && let Some(video) = selected
            {
                app.set_episode_context(SharedString::from(episode_context_label(&video)));
                if thumbnail_changed
                    && !app.get_player_open()
                    && let Some(url) = video.thumbnail.filter(|url| !url.is_empty())
                {
                    let bridge = self.clone();
                    net::fetch_image(url, None, move |pixels| {
                        let Some(pixels) = pixels else {
                            return;
                        };
                        let _ = slint::invoke_from_event_loop(move || {
                            let current = bridge
                                .shared
                                .lock()
                                .unwrap()
                                .modal_item
                                .as_ref()
                                .is_some_and(|modal| {
                                    modal.id == id && modal.request_id == video.id
                                });
                            if current && let Some(app) = bridge.app() {
                                app.set_player_poster(Image::from_rgba8(pixels));
                            }
                        });
                    });
                }
            }
        }
        self.apply_season_cards();
        self.dispatch_season_thumbs();
        self.refresh_episode_rows();
    }

    /// Row index of `episode_id` in the filter-narrowed episode rows of
    /// `season`. None when filtered out / unknown (falls back to defaults).
    pub(super) fn filtered_row_index(
        videos: &[Video],
        season: u32,
        filter: &str,
        episode_id: &str,
    ) -> Option<usize> {
        season_episodes(videos, season)
            .iter()
            .filter(|v| episode_matches_filter(v, filter))
            .position(|v| v.id == episode_id)
    }

    /// Video id of the `index`-th filter-narrowed episode row of `season`.
    pub(super) fn filtered_row_id(
        videos: &[Video],
        season: u32,
        filter: &str,
        index: usize,
    ) -> Option<String> {
        season_episodes(videos, season)
            .iter()
            .filter(|v| episode_matches_filter(v, filter))
            .nth(index)
            .map(|v| v.id.clone())
    }

    /// Snapshot the open modal's position (tab, season, focused episode /
    /// streams, filter, keyboard focus) so reopening the same entry restores
    /// it. No-op when no modal is open.
    pub(super) fn save_detail_snapshot(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let snap = {
            let state = self.shared.lock().unwrap();
            let m = match state.modal_item.as_ref() {
                Some(m) => m,
                None => return,
            };
            let season_val = m.seasons.get(m.season_index).copied();
            let filter = app.get_episode_filter().to_string();
            let kb_ep = app.get_detail_kb_ep().max(0) as usize;
            let episode_id = match season_val {
                Some(season) if app.get_modal_episodes() => {
                    Self::filtered_row_id(&m.videos, season, &filter, kb_ep)
                }
                _ => None,
            };
            // The streams view is deliberately not restored: reopening an
            // entry lands on the episode list (same season), never
            // auto-opens a previously picked episode's streams.
            (
                m.id.clone(),
                DetailSnapshot {
                    tab: app.get_detail_tab(),
                    season: season_val,
                    episode_id,
                    stream_idx: app.get_detail_kb_s().max(0) as usize,
                    stream_request: m.request_id.clone(),
                    filter,
                    kb_zone: app.get_detail_kb_zone(),
                    kb_top: app.get_detail_kb_top(),
                    kb_tab: app.get_detail_kb_tab(),
                    kb_ci: app.get_detail_kb_ci(),
                    kb_ep: app.get_detail_kb_ep(),
                    kb_s: app.get_detail_kb_s(),
                },
            )
        };
        self.shared
            .lock()
            .unwrap()
            .detail_snapshots
            .insert(snap.0, snap.1);
    }

    /// Restore a saved position for `id` (same-entry reopen): tab, filter
    /// and keyboard focus now; season/episode/stream views restore once
    /// their async data arrives (show_episode_picker / apply_streams).
    /// Resets to defaults when no snapshot exists (different entry).
    pub(super) fn restore_detail_snapshot(&self, id: &str) {
        let Some(app) = self.app() else {
            return;
        };
        let snap = self
            .shared
            .lock()
            .unwrap()
            .detail_snapshots
            .get(id)
            .cloned();
        match snap {
            Some(s) => {
                app.set_detail_tab(s.tab);
                app.set_episode_filter(SharedString::from(&s.filter));
                app.set_detail_kb_zone(s.kb_zone);
                app.set_detail_kb_top(s.kb_top);
                app.set_detail_kb_tab(s.kb_tab);
                app.set_detail_kb_ci(s.kb_ci);
                app.set_detail_kb_ep(s.kb_ep);
                app.set_detail_kb_s(s.kb_s);
            }
            None => {
                app.set_detail_tab(0);
                app.set_episode_filter(SharedString::default());
                app.set_detail_kb_zone(0);
                app.set_detail_kb_top(0);
                app.set_detail_kb_tab(0);
                app.set_detail_kb_ci(2);
                app.set_detail_kb_ep(0);
                app.set_detail_kb_s(0);
            }
        }
    }

    /// Main thread: switch the modal to the season/episode picker.
    /// Build the episode-row model for the modal item's currently selected
    /// season, filling decoded thumbnails from the process-wide poster cache.
    /// Parse an ISO 8601 date string and return a human-friendly date.
    /// When `relative` is true, returns relative strings ("3 days ago");
    /// when false, always returns an absolute date ("Mar 15, 2024").
    pub(super) fn format_human_date(iso: &str, relative: bool) -> Option<String> {
        let s = iso.trim();
        // Expect at least "YYYY-MM-DD".
        if s.len() < 10 {
            return None;
        }
        let year: i64 = s[0..4].parse().ok()?;
        let month: i64 = s[5..7].parse().ok()?;
        let day: i64 = s[8..10].parse().ok()?;
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }

        let abs = || text::absolute_date(day as u32, month as u32, year);

        if !relative {
            return Some(abs());
        }

        // Days since Unix epoch for the parsed date.
        let y = year;
        let m = month;
        let d = day;
        let mut days = (y - 1970) * 365 + (y - 1969) / 4 - (y - 1901) / 100 + (y - 1601) / 400;
        let mdays: &[i64] = &[0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
        days += mdays[(m - 1) as usize] + (d - 1);
        // Leap-year adjustment for dates after Feb.
        if m > 2 && (y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)) {
            days += 1;
        }

        // Current date as days since epoch.
        // the web build reads the browser clock via js_sys instead.)
        let now_secs = now_secs();
        let today = (now_secs / 86400) as i64;

        let diff = today - days;

        if diff != 0 {
            // Relative wording (plural forms included) is language-specific —
            // see `text::relative_days`.
            return Some(text::relative_days(diff));
        }

        // Fallback: absolute date.
        Some(abs())
    }

    /// Grid-card synopsis: the episode overview truncated to `max_chars` at
    /// a word boundary (empty when the addon sent no overview). Grid cards
    /// already show the release date on its own line, so the date is left
    /// out here.
    pub(super) fn truncate_synopsis(text: &str, max_chars: usize) -> String {
        // Collapse all interior whitespace (newlines, tabs, runs of
        // spaces): addon overviews often contain paragraph breaks, and a
        // grid card reserves a preferred height per explicit line break
        // even when it paints a single elided line — leaving tall blank
        // areas on those cards.
        let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.chars().count() <= max_chars {
            return collapsed;
        }
        let cut: String = collapsed.chars().take(max_chars).collect();
        match cut.rfind([' ', '\n', '\t']) {
            Some(i) => format!("{}…", cut[..i].trim_end()),
            None => format!("{cut}…"),
        }
    }

    /// Episode-card synopsis: the overview truncated to 150 chars total
    /// *including* the resume suffix, which is appended afterwards.
    /// Budgeting the truncation against the combined string (not the
    /// overview alone) keeps the card text inside the 3-line window
    /// the .slint side reserves at any column count: without this the
    /// suffix chars could push a borderline 3-line synopsis onto a
    /// 4th line that the card then clips mid-glyph.
    pub(super) fn episode_details(overview: Option<&str>, resume_suffix: &str) -> String {
        const EPISODE_SYNOPSIS_CHARS: usize = 150;
        let budget = EPISODE_SYNOPSIS_CHARS.saturating_sub(resume_suffix.chars().count());
        let mut details = overview
            .map(|o| Self::truncate_synopsis(o, budget))
            .unwrap_or_default();
        details.push_str(resume_suffix);
        details
    }

    /// Whether a series is finished, from the meta `status` field
    /// (Cinemeta sends "Ended"/"Continuing"; other addons vary).
    /// Unknown or missing status counts as unfinished: a refresh run
    /// is cheap, while treating an ongoing show as ended would serve
    /// stale episode data and posters indefinitely.
    pub(super) fn series_finished(status: Option<&str>) -> bool {
        status.is_some_and(|s| s.eq_ignore_ascii_case("ended"))
    }

    /// Watched/progress/resume-suffix triple for one episode, shared by row
    /// rendering and thumbnail queueing so both agree on what "started" means.
    pub(super) fn episode_watch_state(
        map: &HashMap<String, EpisodeProgress>,
        series_id: &str,
        episode_id: &str,
    ) -> (bool, f32, String) {
        match map.get(&progress_map_key(series_id, episode_id)) {
            Some(p) if p.watched => (true, 1.0f32, String::new()),
            Some(p) if resumable_position(p.position_secs, p.duration_secs, p.watched) => (
                false,
                progress_fraction(p.position_secs, p.duration_secs),
                format!(
                    " · {} {}",
                    text::tr("▶ Resume"),
                    crate::player::format_time(p.position_secs)
                ),
            ),
            Some(p) => (
                false,
                progress_fraction(p.position_secs, p.duration_secs),
                String::new(),
            ),
            None => (false, 0.0, String::new()),
        }
    }

    /// Whether an episode row may show its thumbnail under the
    /// `show_unwatched_thumbs` setting: hidden only for fresh episodes with
    /// no watch flag and no playback position.
    pub(super) fn episode_thumb_visible(
        show_unwatched: bool,
        watched: bool,
        progress: f32,
    ) -> bool {
        show_unwatched || watched || progress > 0.0
    }

    pub(super) fn current_episode_rows(&self) -> Vec<EpisodeRow> {
        struct RowSeed {
            text: SharedString,
            details: String,
            thumb_url: Option<String>,
            show_thumb: bool,
            watched: bool,
            progress: f32,
            ep_no: SharedString,
            date: SharedString,
        }
        let seeds: Vec<RowSeed> = {
            let state = self.shared.lock().unwrap();
            let Some(m) = state.modal_item.as_ref() else {
                return Vec::new();
            };
            if m.seasons.is_empty() {
                return Vec::new();
            }
            let date_relative = state.cache_settings.date_relative;
            let show_unwatched = state.cache_settings.show_unwatched_thumbs;
            let season = m.seasons[m.season_index.min(m.seasons.len() - 1)];
            // The card filter box narrows the same season list the picker
            // shows (episode_picked resolves through this same filter).
            let filter = self
                .app()
                .map(|a| a.get_episode_filter().to_string())
                .unwrap_or_default();
            season_episodes(&m.videos, season)
                .iter()
                .filter(|v| episode_matches_filter(v, &filter))
                .map(|v| {
                    let (watched, progress, resume_suffix) =
                        Self::episode_watch_state(&state.progress, &m.id, &v.id);
                    let label = episode_row_label(v);
                    // No checkmark prefix: watched state already shows on the
                    // thumbnail disc overlay plus the dimmed title color.
                    let text = SharedString::from(label);
                    // Grid cards show the date on its own line, so the
                    // synopsis is the (truncated) overview only. The card
                    // reserves exactly 3 lines (see detail.slint); this
                    // Rust-side truncation keeps the text short so the
                    // renderer ellipsis rarely has to do any work.
                    let details = Self::episode_details(v.overview.as_deref(), &resume_suffix);
                    let date = v
                        .released
                        .as_deref()
                        .and_then(|d| {
                            Self::format_human_date(d, date_relative)
                                .or_else(|| Some(d.trim().to_string()))
                        })
                        .unwrap_or_default();
                    RowSeed {
                        text,
                        details,
                        thumb_url: v.thumbnail.clone(),
                        show_thumb: Self::episode_thumb_visible(show_unwatched, watched, progress),
                        watched,
                        progress,
                        ep_no: SharedString::from(episode_badge(v)),
                        date: SharedString::from(date),
                    }
                })
                .collect()
        };

        seeds
            .into_iter()
            .map(|seed| {
                let reserved_lines = Self::estimate_stream_lines(&seed.text)
                    + if seed.details.is_empty() {
                        0
                    } else {
                        Self::estimate_stream_lines(&seed.details)
                    };
                let lines = reserved_lines.max(1);
                // Episode rows display the downscaled variant, which the
                // pump stores under the sized key (see EPISODE_THUMB_SIDE).
                // Hidden by the `show_unwatched_thumbs` setting for fresh
                // episodes (placeholder instead).
                let (thumb, has_thumb) = if !seed.show_thumb {
                    (Image::default(), false)
                } else {
                    match &seed.thumb_url {
                        Some(url) => {
                            match decoded_cache_get(&sized_cache_key(url, Some(EPISODE_THUMB_SIDE)))
                            {
                                Some(buf) => (Image::from_rgba8(buf), true),
                                None => (Image::default(), false),
                            }
                        }
                        None => (Image::default(), false),
                    }
                };
                EpisodeRow {
                    text: seed.text,
                    details: SharedString::from(&seed.details),
                    lines,
                    thumb,
                    has_thumb,
                    watched: seed.watched,
                    progress: seed.progress,
                    ep_no: seed.ep_no,
                    date: seed.date,
                }
            })
            .collect()
    }

    /// (Re)build + push the episode rows for the current season and start
    /// downloading any thumbnails that are not cached yet.
    /// Render the episode list from the current season state (thumbnails
    /// already present in the decode cache are filled in).
    pub(super) fn apply_episode_rows(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let all = self.current_episode_rows();
        let total = all.len();
        // Clamp the stored page: the filter/season may have shrunk the list
        // while it was showing a later page.
        let page = {
            let mut state = self.shared.lock().unwrap();
            match state.modal_item.as_mut() {
                Some(m) => {
                    m.episode_page = m.episode_page.min(page_count(total).saturating_sub(1));
                    m.episode_page
                }
                None => 0,
            }
        };
        let start = page * EPISODE_PAGE_SIZE;
        let rows: Vec<EpisodeRow> = all
            .into_iter()
            .skip(start)
            .take(EPISODE_PAGE_SIZE)
            .collect();
        app.set_episode_rows(Rc::new(VecModel::from(rows)).into());
        app.set_episode_page(page as i32);
        app.set_episode_page_count(page_count(total) as i32);
        app.set_episode_page_start(start as i32);
        app.set_episode_total(total as i32);
    }

    /// Main thread: the Episodes tab pager moved to `page` (absolute; clamped
    /// here). Rebuilds the page's rows and queues its thumbnails.
    pub(super) fn episode_page_picked(&self, page: i32) {
        self.cancel_watch_now();
        let total = self.current_episode_rows().len();
        let pages = page_count(total) as i32;
        let page = page.clamp(0, pages.saturating_sub(1));
        let moved = {
            let mut state = self.shared.lock().unwrap();
            match state.modal_item.as_mut() {
                Some(m) if m.episode_page != page as usize => {
                    m.episode_page = page as usize;
                    true
                }
                _ => false,
            }
        };
        if !moved {
            return;
        }
        // A new page starts at its first card.
        if let Some(app) = self.app() {
            app.set_detail_kb_ep(0);
        }
        self.refresh_episode_rows();
    }

    /// Point the Episodes tab at the page holding `index` (a global filtered
    /// index) and return that index's position inside the page. Rebuilds the
    /// rows when the page moved — Home's resume row can pick an episode from
    /// another page.
    fn episode_page_for_index(&self, index: usize) -> usize {
        let page = index / EPISODE_PAGE_SIZE;
        let moved = {
            let mut state = self.shared.lock().unwrap();
            match state.modal_item.as_mut() {
                Some(m) if m.episode_page != page => {
                    m.episode_page = page;
                    true
                }
                _ => false,
            }
        };
        if moved {
            self.apply_episode_rows();
        }
        index % EPISODE_PAGE_SIZE
    }

    /// Back to the first page (season or episode filter changed).
    fn reset_episode_page(&self) {
        if let Some(m) = self.shared.lock().unwrap().modal_item.as_mut() {
            m.episode_page = 0;
        }
    }

    /// (Re)build + push the episode rows for the current season and queue
    /// thumbnail downloads. Big seasons are kept responsive by the capped
    /// download pool + debounced re-renders below.
    pub(super) fn refresh_episode_rows(&self) {
        // Cancel queued downloads that belonged to a previous list.
        EPISODE_QUEUE.lock().unwrap().clear();
        EPISODE_INFLIGHT.lock().unwrap().clear();
        self.apply_episode_rows();
        self.queue_episode_thumbnails();
    }

    /// Season picker cards for the open modal, in `seasons` order. Artwork
    /// reads from the same sized decode-cache tier as the episode
    /// thumbnails, so cards and episode rows share pixels with no duplicate
    /// downloads. Unlike episode rows the art always shows (it decorates
    /// the picker like the show backdrop, regardless of watch state).
    pub(super) fn current_season_cards(&self) -> Vec<SeasonCard> {
        let state = self.shared.lock().unwrap();
        let Some(m) = state.modal_item.as_ref() else {
            return Vec::new();
        };
        m.seasons
            .iter()
            .map(|&s| {
                let (thumb, has_thumb) =
                    match season_thumb_url(&m.videos, s).as_deref().and_then(|url| {
                        decoded_cache_get(&sized_cache_key(url, Some(EPISODE_THUMB_SIDE)))
                    }) {
                        Some(buf) => (Image::from_rgba8(buf), true),
                        None => (Image::default(), false),
                    };
                // Watched fraction from the progress map (manual toggles and
                // playback both land here). Every known episode counts,
                // including unaired ones: a season with episodes still to
                // come never reads fully watched.
                let eps = season_episodes(&m.videos, s);
                let total = eps.len();
                let watched_n = eps
                    .iter()
                    .filter(|v| {
                        state
                            .progress
                            .get(&progress_map_key(&m.id, &v.id))
                            .is_some_and(|p| p.watched)
                    })
                    .count();
                SeasonCard {
                    name: SharedString::from(season_label(s)),
                    thumb,
                    has_thumb,
                    watched: total > 0 && watched_n == total,
                    progress: if total > 0 {
                        watched_n as f32 / total as f32
                    } else {
                        0.0
                    },
                }
            })
            .collect()
    }

    /// Push the current season cards to the UI (empty without an open modal).
    pub(super) fn apply_season_cards(&self) {
        if let Some(app) = self.app() {
            app.set_season_cards(Rc::new(VecModel::from(self.current_season_cards())).into());
        }
    }

    pub(super) fn show_episode_picker(&self, id: String, videos: Vec<Video>) {
        let seasons = ordered_seasons(&videos);
        if seasons.is_empty() {
            self.resolve_watch_now();
            self.start_stream_search(id);
            return;
        }
        // Same-entry reopen: restore the saved season instead of season 0.
        let snapshot = self
            .shared
            .lock()
            .unwrap()
            .detail_snapshots
            .get(&id)
            .cloned();
        let (season_names, season_idx) = {
            let mut state = self.shared.lock().unwrap();
            let m = match state.modal_item.as_mut() {
                Some(m) if m.id == id => m,
                _ => return,
            };
            m.videos = videos;
            m.seasons = seasons;
            m.season_index = snapshot
                .as_ref()
                .and_then(|s| s.season)
                .and_then(|sv| m.seasons.iter().position(|&x| x == sv))
                .unwrap_or(0);
            let idx = m.season_index;
            let names = m
                .seasons
                .iter()
                .map(|&s| SharedString::from(season_label(s)))
                .collect::<Vec<_>>();
            (names, idx)
        };
        if let Some(app) = self.app() {
            app.set_modal_episodes(true);
            app.set_detail_deep_stream(false);
            app.set_episode_context(SharedString::default());
            app.set_season_names(Rc::new(VecModel::from(season_names)).into());
            app.set_season_combo_idx(season_idx as i32);
            self.clear_streams();
            self.set_stream_hint(None);
            // Series land on the Episodes tab; a same-entry reopen restores
            // its saved tab instead (e.g. Overview).
            app.set_detail_tab(snapshot.as_ref().map(|s| s.tab).unwrap_or(3));
        }
        self.apply_season_cards();
        self.dispatch_season_thumbs();
        self.refresh_episode_rows();
        // Restore the focused episode row (resolved by id against the rows
        // just built, so refreshes/reorderings fall back gracefully).
        if let Some(snap) = snapshot.as_ref()
            && let Some(ep_id) = snap.episode_id.as_ref()
        {
            let idx = {
                let state = self.shared.lock().unwrap();
                state.modal_item.as_ref().and_then(|m| {
                    let season = m.seasons.get(m.season_index).copied()?;
                    let filter = self
                        .app()
                        .map(|a| a.get_episode_filter().to_string())
                        .unwrap_or_default();
                    Self::filtered_row_index(&m.videos, season, &filter, ep_id)
                })
            };
            if let (Some(app), Some(i)) = (self.app(), idx) {
                app.set_detail_kb_ep(i as i32);
            }
        }
        // Cached metadata can satisfy Watch Now before the refresh answers.
        self.resolve_watch_now();
    }

    /// Main thread: switch the episode list to another season.
    pub(super) fn season_picked(&self, index: usize) {
        self.cancel_watch_now();
        self.reset_episode_page();
        let idx = {
            let mut state = self.shared.lock().unwrap();
            let m = match state.modal_item.as_mut() {
                Some(m) if !m.seasons.is_empty() => m,
                _ => return,
            };
            m.season_index = index.min(m.seasons.len() - 1);
            m.season_index
        };
        if let Some(app) = self.app() {
            // The season grid highlight follows this (the old dropdown
            // held its own visual selection; the grid is property-driven).
            app.set_season_combo_idx(idx as i32);
            app.set_detail_kb_ci(0);
            app.set_detail_kb_ep(0);
        }
        self.refresh_episode_rows();
    }

    /// Resolve a displayed (filter-narrowed) episode row index to
    /// `(series_id, episode_id, ordered episode ids of the season)`.
    /// Uses the same season + filter as `current_episode_rows`, so menu
    /// actions and the quick-toggle always hit the card the user touched.
    pub(super) fn resolve_filtered_episode(
        &self,
        index: usize,
    ) -> Option<(String, String, Vec<String>)> {
        let state = self.shared.lock().unwrap();
        let m = state.modal_item.as_ref()?;
        if m.seasons.is_empty() {
            return None;
        }
        let season = m.seasons[m.season_index.min(m.seasons.len() - 1)];
        let filter = self
            .app()
            .map(|a| a.get_episode_filter().to_string())
            .unwrap_or_default();
        let season_eps = season_episodes(&m.videos, season);
        let ordered: Vec<String> = season_eps.iter().map(|v| v.id.clone()).collect();
        let video = *season_eps
            .iter()
            .filter(|v| episode_matches_filter(v, &filter))
            .nth(index)?;
        Some((m.id.clone(), video.id.clone(), ordered))
    }

    /// Set one episode's watched flag, creating the entry when needed.
    /// Marking watched keeps a known position/duration so un-toggling
    /// restores the resume rail; unknown durations stay 0 (the row still
    /// renders ✓ via the watched flag). Marking unwatched clears the
    /// saved position back to fresh.
    pub(super) fn set_episode_watched_locked(
        map: &mut HashMap<String, EpisodeProgress>,
        series_id: &str,
        episode_id: &str,
        watched: bool,
    ) {
        let key = progress_map_key(series_id, episode_id);
        let now = now_secs();
        if watched {
            let entry = map.entry(key).or_insert_with(|| EpisodeProgress {
                series_id: series_id.to_string(),
                episode_id: episode_id.to_string(),
                ..Default::default()
            });
            entry.series_id = series_id.to_string();
            entry.episode_id = episode_id.to_string();
            entry.watched = true;
            // A fresh watch intent supersedes any earlier unwatch, so the
            // mesh merge can't mistake the stale unwatch for current intent.
            entry.unwatched_at_secs = 0;
            entry.updated_at_secs = now;
        } else if let Some(entry) = map.get_mut(&key) {
            entry.watched = false;
            entry.position_secs = 0.0;
            // Explicit unwatch intent: newer than the other side's state it
            // wins the mesh merge even against a stale `watched`.
            entry.unwatched_at_secs = now;
            entry.updated_at_secs = now;
        }
    }

    /// Main thread: quick-toggle one episode's watched flag (`✓` overlay,
    /// `W` key). `index` is into the filtered rows.
    pub(super) fn episode_toggle_watched(&self, index: usize) {
        let Some((series_id, episode_id, _)) = self.resolve_filtered_episode(index) else {
            return;
        };
        let now_watched = {
            let mut state = self.shared.lock().unwrap();
            let now_watched = !state
                .progress
                .get(&progress_map_key(&series_id, &episode_id))
                .is_some_and(|p| p.watched);
            Self::set_episode_watched_locked(
                &mut state.progress,
                &series_id,
                &episode_id,
                now_watched,
            );
            now_watched
        };
        if now_watched {
            self.auto_delete_watched_downloads(&series_id, &[episode_id]);
        }
        self.persist_and_refresh_progress();
    }

    /// Main thread: episode context-menu action. `index` is into the
    /// filtered rows; `action`: 1 mark watched, 2 mark unwatched,
    /// 3 mark this + all previous (season order) watched, 4 clear resume
    /// position (keeps an existing watched flag).
    pub(super) fn episode_watch_action(&self, index: usize, action: i32) {
        let Some((series_id, episode_id, ordered)) = self.resolve_filtered_episode(index) else {
            return;
        };
        let mut watched_ids: Vec<String> = Vec::new();
        {
            let mut state = self.shared.lock().unwrap();
            match action {
                1 => {
                    Self::set_episode_watched_locked(
                        &mut state.progress,
                        &series_id,
                        &episode_id,
                        true,
                    );
                    watched_ids.push(episode_id.clone());
                }
                2 => Self::set_episode_watched_locked(
                    &mut state.progress,
                    &series_id,
                    &episode_id,
                    false,
                ),
                3 => {
                    // Mark this + all previous watched, skipping unaired
                    // episodes (known future air date).
                    let today = today_days();
                    let upto = ordered.iter().position(|id| id == &episode_id);
                    let ids: Vec<String> = match upto {
                        Some(pos) => {
                            let videos = state
                                .modal_item
                                .as_ref()
                                .map(|m| m.videos.clone())
                                .unwrap_or_default();
                            ordered
                                .iter()
                                .take(pos + 1)
                                .filter(|id| {
                                    videos
                                        .iter()
                                        .find(|v| &v.id == *id)
                                        .map(|v| episode_is_out(v, today))
                                        .unwrap_or(true)
                                })
                                .cloned()
                                .collect()
                        }
                        None => vec![episode_id.clone()],
                    };
                    for id in &ids {
                        Self::set_episode_watched_locked(&mut state.progress, &series_id, id, true);
                    }
                    watched_ids.extend(ids);
                }
                4 => {
                    let key = progress_map_key(&series_id, &episode_id);
                    if let Some(entry) = state.progress.get_mut(&key) {
                        entry.position_secs = 0.0;
                        entry.updated_at_secs = now_secs();
                    }
                }
                _ => return,
            }
        }
        if !watched_ids.is_empty() {
            self.auto_delete_watched_downloads(&series_id, &watched_ids);
        }
        self.persist_and_refresh_progress();
    }

    /// Main thread: season context-menu action. `index` is into the season
    /// wall (unfiltered); `action`: 0 toggle whole season (unwatch when
    /// fully watched, else watch all), 1 mark watched, 2 mark unwatched.
    /// Watching only ever touches released episodes — unaired ones stay
    /// untouched (they surface on Home → Upcoming instead).
    pub(super) fn season_watch_action(&self, index: usize, action: i32) {
        let today = today_days();
        let (series_id, episode_ids, watched_flags): (String, Vec<String>, Vec<bool>) = {
            let state = self.shared.lock().unwrap();
            let Some(m) = state.modal_item.as_ref() else {
                return;
            };
            if m.seasons.is_empty() {
                return;
            }
            let Some(&season) = m.seasons.get(index) else {
                return;
            };
            // Bulk actions skip unaired episodes (known future air date).
            let eps: Vec<&Video> = season_episodes(&m.videos, season)
                .into_iter()
                .filter(|v| episode_is_out(v, today))
                .collect();
            let ids = eps.iter().map(|v| v.id.clone()).collect::<Vec<_>>();
            let flags = eps
                .iter()
                .map(|v| {
                    state
                        .progress
                        .get(&progress_map_key(&m.id, &v.id))
                        .is_some_and(|p| p.watched)
                })
                .collect::<Vec<_>>();
            (m.id.clone(), ids, flags)
        };
        if episode_ids.is_empty() {
            return;
        }
        let target = match action {
            1 => true,
            2 => false,
            // Toggle: unwatch when fully watched, else watch everything
            // released (unaired episodes were filtered above).
            0 => !watched_flags.iter().all(|&w| w),
            _ => return,
        };
        {
            let mut state = self.shared.lock().unwrap();
            for id in &episode_ids {
                Self::set_episode_watched_locked(&mut state.progress, &series_id, id, target);
            }
        }
        if target {
            self.auto_delete_watched_downloads(&series_id, &episode_ids);
        }
        self.persist_and_refresh_progress();
    }

    /// Main thread: an episode was picked — show its streams. `index` is
    /// into the currently displayed (filter-narrowed) episode rows.
    pub(super) fn episode_picked(&self, index: usize) {
        self.cancel_watch_now();
        let (series_id, request_id, context, thumb_url) = {
            let state = self.shared.lock().unwrap();
            let m = match state.modal_item.as_ref() {
                Some(m) if !m.seasons.is_empty() => m,
                _ => return,
            };
            let season = m.seasons[m.season_index];
            // Same filter current_episode_rows applies, so a filtered card
            // opens its own episode (row order matches the list).
            let filter = self
                .app()
                .map(|a| a.get_episode_filter().to_string())
                .unwrap_or_default();
            let season_eps = season_episodes(&m.videos, season);
            let episodes: Vec<&&Video> = season_eps
                .iter()
                .filter(|v| episode_matches_filter(v, &filter))
                .collect();
            let video = match episodes.get(index) {
                Some(v) => v,
                None => return,
            };
            (
                m.id.clone(),
                video.id.clone(),
                episode_context_label(video),
                video.thumbnail.clone(),
            )
        };
        // Arm playback tracking for this episode, resolving a resume point
        // from history (fresh when watched / no saved position).
        {
            let mut state = self.shared.lock().unwrap();
            let resume_pos = state
                .progress
                .get(&progress_map_key(&series_id, &request_id))
                .filter(|p| resumable_position(p.position_secs, p.duration_secs, p.watched))
                .map(|p| p.position_secs);
            state.playback = Some(PlaybackTarget {
                series_id,
                episode_id: request_id.clone(),
                resume_pos,
                ..Default::default()
            });
        }
        if let Some(app) = self.app() {
            app.set_modal_episodes(false);
            // A manual pick means the episode list was visited, so any
            // deep-link back shortcut ends here (`continue_picked` sets it
            // again afterwards for its own auto-opened streams).
            app.set_detail_deep_stream(false);
            app.set_episode_context(SharedString::from(&context));
            self.clear_streams();
            self.set_stream_hint(Some(StreamHint::Fixed("Loading streams…")));
            // Streams render inside the Episodes tab for series.
            app.set_detail_tab(3);
            // The picked episode can live on another page (Home's resume row):
            // move the pager there first, so the row model below matches the
            // index. kb state and row lookups are page-local.
            let local_index = self.episode_page_for_index(index);
            // Keyboard position sync (uniform for mouse + keyboard picks;
            // Slint click handlers only clear kb_active).
            app.set_detail_kb_ci(2);
            app.set_detail_kb_ep(local_index as i32);
            app.set_detail_kb_s(0);
            // Use the episode's thumbnail as the player's loading-screen
            // background once it has decoded (row order matches the list).
            if let Some(row) = app.get_episode_rows().row_data(local_index)
                && row.has_thumb
            {
                app.set_player_poster(row.thumb);
            }
        }
        // Full-res backdrop for the player loading screen: the list rows
        // carry downscaled thumbs, so fetch the original and upgrade the
        // poster — but only if the user hasn't moved on meanwhile.
        if let Some(url) = thumb_url {
            let ctx = context.clone();
            let app_weak = self.app.clone();
            net::fetch_image(url, None, move |pixels| {
                let Some(pixels) = pixels else {
                    return;
                };
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(app) = app_weak.upgrade() else {
                        return;
                    };
                    if app.get_episode_context().as_str() != ctx.as_str() {
                        return; // picked another episode since
                    }
                    app.set_player_poster(Image::from_rgba8(pixels));
                });
            });
        }
        self.start_stream_search(request_id);
    }

    /// Main thread: go back from an episode's streams to the episode list.
    pub(super) fn episodes_back(&self) {
        self.cancel_watch_now();
        let is_episodic = {
            let state = self.shared.lock().unwrap();
            state
                .modal_item
                .as_ref()
                .map(|m| !m.seasons.is_empty())
                .unwrap_or(false)
        };
        if !is_episodic {
            return;
        }
        if let Some(app) = self.app() {
            app.set_modal_episodes(true);
            // The episode list is on screen now, so a deep-link back shortcut
            // no longer applies (system back returns here from new streams).
            app.set_detail_deep_stream(false);
            app.set_episode_context(SharedString::default());
            self.clear_streams();
            self.set_stream_hint(None);
            // Return to the Episodes tab (streams live there for series).
            app.set_detail_tab(3);
            app.set_detail_kb_zone(3);
            app.set_detail_kb_ci(2);
        }
    }

    /// Detail tab bar: 0 = Overview, 3 = Episodes (1/2 are addon-less stubs).
    /// Series episode streams render inside the Episodes tab, so stepping
    /// into/out of streams keeps the tab in sync.
    pub(super) fn detail_tab_picked(&self, tab: i32) {
        self.cancel_watch_now();
        let Some(app) = self.app() else {
            return;
        };
        let tab = tab.clamp(0, 3);
        app.set_detail_tab(tab);
        app.set_detail_kb_tab(if tab == 3 { 3 } else { 0 });
    }

    /// Episode filter box: re-render the (filtered) rows + thumbnails.
    pub(super) fn episode_filter_changed(&self, text: SharedString) {
        self.cancel_watch_now();
        self.reset_episode_page();
        if let Some(app) = self.app() {
            app.set_episode_filter(text);
            app.set_detail_kb_ci(1);
            app.set_detail_kb_ep(0);
        }
        self.refresh_episode_rows();
    }

    /// Both Watch Now entry points share the same episode selection; streams
    /// remain a manual choice so quality/addon preferences are respected.
    pub(super) fn watch_now(&self) {
        {
            let mut state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_mut() else {
                return;
            };
            if modal.type_ == "movie" {
                return;
            }
            modal.pending_watch_now = Some(WatchNowOrigin::Detail);
        }
        if let Some(app) = self.app() {
            app.set_detail_tab(3);
            app.set_detail_kb_zone(3);
        }
        self.resolve_watch_now();
    }

    fn cancel_watch_now(&self) {
        if let Some(modal) = self.shared.lock().unwrap().modal_item.as_mut() {
            modal.pending_watch_now = None;
        }
    }

    /// Consume one pending intent once an eligible episode is known. With
    /// no cached candidate, keep waiting for the fresh metadata lookup.
    fn resolve_watch_now(&self) {
        let selected = {
            let mut state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_ref() else {
                return;
            };
            let Some(origin) = modal.pending_watch_now else {
                return;
            };
            let target = next_episode_to_watch(&modal.id, &modal.videos, &state.progress).and_then(
                |video| {
                    let season = video.season?;
                    let row = Self::filtered_row_index(&modal.videos, season, "", &video.id)?;
                    let season_index = modal.seasons.iter().position(|&s| s == season)?;
                    Some((season_index, row))
                },
            );
            if target.is_none() && modal.episodes_loading {
                return;
            }
            let modal = state.modal_item.as_mut().unwrap();
            modal.pending_watch_now = None;
            target.map(|(season_index, row)| {
                modal.season_index = season_index;
                modal.episode_page = 0;
                (season_index, row, origin)
            })
        };
        let Some((season_index, row, origin)) = selected else {
            self.episodes_back();
            return;
        };
        let Some(app) = self.app() else {
            return;
        };
        app.set_episode_filter(SharedString::default());
        app.set_season_combo_idx(season_index as i32);
        self.refresh_episode_rows();
        self.episode_picked(row);
        if matches!(origin, WatchNowOrigin::Featured)
            && !app.get_modal_episodes()
            && !app.get_episode_context().is_empty()
        {
            app.set_detail_deep_stream(true);
        }
    }

    /// Main thread: display the stream rows for the still-open modal item
    /// whose current stream request matches `request_id`.
    /// Reserve the number of text lines a stream row will need: its explicit
    /// newline lines plus a conservative estimate of word-wrapped lines, so the
    /// UI can give each card an explicit height that always fits the text.
    pub(super) fn estimate_stream_lines(text: &str) -> i32 {
        // ~90 latin chars fit one line at the default window width, so a
        // smaller constant over-counts wrapped lines and makes rows too tall.
        const CHARS_PER_LINE: usize = 90;
        let mut total: i32 = 0;
        for line in text.split('\n') {
            let chars = line.chars().count();
            let wraps = if chars == 0 {
                0
            } else {
                chars.div_ceil(CHARS_PER_LINE)
            };
            total += wraps as i32;
        }
        total.max(1)
    }

    /// One addon answered the stream query: merge its rows into the list and
    /// refresh the UI immediately. An empty `new_rows` drops the addon's pill
    /// (it returned nothing).
    pub(super) fn stream_addon_loaded(
        &self,
        request_id: String,
        generation: u64,
        addon: String,
        new_rows: Vec<StreamUi>,
    ) {
        let (was_empty, addons, loading, searching, filter) = {
            let mut state = self.shared.lock().unwrap();
            if !stream_response_is_current(&state, &request_id, generation) {
                return;
            }
            let was_empty = state.stream_all.is_empty();
            state.stream_pending.retain(|p| p != &addon);
            if new_rows.is_empty() {
                // Returned nothing: remove its pill and fall back to All if it
                // was the active filter.
                if let Some(pos) = state.stream_addons.iter().position(|a| a == &addon) {
                    state.stream_addons.remove(pos);
                    let selected = state.stream_filter.checked_sub(1).unwrap_or(usize::MAX);
                    if selected == pos || state.stream_filter > state.stream_addons.len() {
                        state.stream_filter = 0;
                    }
                }
            } else {
                state.stream_all.extend(new_rows);
            }
            let addons: Vec<SharedString> =
                state.stream_addons.iter().map(SharedString::from).collect();
            let loading: Vec<bool> = state
                .stream_addons
                .iter()
                .map(|a| state.stream_pending.contains(a))
                .collect();
            let searching = !state.stream_pending.is_empty();
            let filter = state.stream_filter;
            (was_empty, addons, loading, searching, filter)
        };

        self.apply_stream_filter();
        if let Some(app) = self.app() {
            app.set_stream_addons(Rc::new(VecModel::from(addons)).into());
            app.set_stream_loading(Rc::new(VecModel::from(loading)).into());
            app.set_streams_searching(searching);
            app.set_stream_filter(filter as i32);
            // Same-entry reopen: restore the saved stream focus once the
            // first results land (never fights the user's own navigation).
            if was_empty {
                let restore_s = {
                    let state = self.shared.lock().unwrap();
                    match state.modal_item.as_ref() {
                        Some(m) => state
                            .detail_snapshots
                            .get(&m.id)
                            .filter(|s| s.stream_request == request_id)
                            .map(|s| s.stream_idx),
                        None => None,
                    }
                };
                if let Some(i) = restore_s {
                    let count = self.shared.lock().unwrap().streams.len();
                    app.set_detail_kb_s(i.min(count.saturating_sub(1)) as i32);
                }
            }
        }
    }

    /// Build the displayed stream list from `stream_all`, narrowed by the
    /// active per-addon filter: the pinned download rows for this request
    /// first, then the filtered addon rows, plus the status hint.
    fn compute_stream_view(&self) -> (Vec<StreamUi>, Vec<StreamRow>, String) {
        let (filtered, filter, addon_count, total, pending_count, addon_label, request_id) = {
            let state = self.shared.lock().unwrap();
            let request_id = state
                .modal_item
                .as_ref()
                .map(|m| m.request_id.clone())
                .unwrap_or_default();
            let filter = state.stream_filter;
            let selected = filter
                .checked_sub(1)
                .and_then(|i| state.stream_addons.get(i))
                .cloned();
            let mut filtered: Vec<StreamUi> = state
                .stream_all
                .iter()
                .filter(|s| filter == 0 || selected.as_deref().is_some_and(|a| a == s.addon))
                .cloned()
                .collect();
            // "All" always lists streams grouped by addon priority (the
            // installed addon order), regardless of which addon answered
            // first. Within an addon the arrival order is kept (stable sort).
            if filter == 0 {
                sort_streams_by_addon(&mut filtered, &state.stream_addons);
            }
            (
                filtered,
                filter,
                state.stream_addons.len(),
                state.stream_all.len(),
                state.stream_pending.len(),
                selected,
                request_id,
            )
        };

        let hint = if pending_count > 0 {
            text::searching_streams(pending_count)
        } else if total == 0 {
            text::tr("No streams found.").to_string()
        } else if filter == 0 {
            text::streams_found(total, addon_count)
        } else {
            match addon_label.as_deref() {
                Some(label) => text::streams_found_from(filtered.len(), label),
                None => text::streams_found_single(filtered.len()),
            }
        };
        let mut displayed: Vec<StreamUi> = self
            .downloads
            .jobs_for_request(&request_id)
            .into_iter()
            .map(|job| {
                let path = job.artifact_path.clone().unwrap_or_default();
                StreamUi {
                    id: format!("download:{}", job.id),
                    display: job.display.clone(),
                    source: StreamSource::Downloaded {
                        job_id: job.id.clone(),
                        path,
                    },
                    addon: job.addon.clone(),
                    download: Some(job),
                }
            })
            .collect();
        displayed.extend(filtered);
        let stream_rows: Vec<StreamRow> = displayed
            .iter()
            .map(|row| {
                let details = row
                    .download
                    .as_ref()
                    .map(DownloadCoordinator::status_text)
                    .unwrap_or_default();
                let lines = Self::estimate_stream_lines(&row.display)
                    + if details.is_empty() { 0 } else { 1 };
                StreamRow {
                    id: SharedString::from(&row.id),
                    text: SharedString::from(&row.display),
                    details: SharedString::from(details),
                    lines,
                    is_download: row.download.is_some(),
                    download_progress: row
                        .download
                        .as_ref()
                        .map(DownloadCoordinator::progress_fraction)
                        .unwrap_or(0.0),
                    download_action: row
                        .download
                        .as_ref()
                        .map(DownloadCoordinator::action_kind)
                        .unwrap_or(0),
                }
            })
            .collect();
        (displayed, stream_rows, hint)
    }

    /// Push the stream rows to the UI (rows + hint).
    ///
    /// Progress ticks call this several times a second while a download runs.
    /// Replacing the whole `VecModel` each time destroys every row delegate —
    /// including the per-row hold timer that opens the action sheet on
    /// Android — so a hold can never reach its threshold and the sheet never
    /// appears. When the row identity/order is unchanged (the common
    /// progress-only case) update the existing rows in place via
    /// `set_row_data`, which preserves the delegates and their timers; only
    /// rebuild the model when the shape actually changes.
    pub(super) fn apply_stream_filter(&self) {
        let (displayed, stream_rows, hint) = self.compute_stream_view();
        let (page, page_count, page_start, total, page_rows) = {
            let mut state = self.shared.lock().unwrap();
            let page_count = stream_rows.len().div_ceil(STREAM_PAGE_SIZE).max(1);
            state.stream_page = state.stream_page.min(page_count - 1);
            let page = state.stream_page;
            let start = page * STREAM_PAGE_SIZE;
            let end = (start + STREAM_PAGE_SIZE).min(stream_rows.len());
            state.streams = displayed;
            state.stream_hint = None;
            (
                page,
                page_count,
                start,
                stream_rows.len(),
                stream_rows[start..end].to_vec(),
            )
        };
        let Some(app) = self.app() else {
            return;
        };
        app.set_stream_page(page as i32);
        app.set_stream_page_count(page_count as i32);
        app.set_stream_page_start(page_start as i32);
        app.set_stream_total(total as i32);
        let model = app.get_streams();
        let existing = model.as_any().downcast_ref::<VecModel<StreamRow>>();
        let same_shape = existing.is_some_and(|model| stream_rows_same_shape(model, &page_rows));
        if same_shape {
            if let Some(model) = existing {
                for (index, row) in page_rows.into_iter().enumerate() {
                    if model.row_data(index).as_ref() != Some(&row) {
                        model.set_row_data(index, row);
                    }
                }
            }
        } else {
            app.set_streams(Rc::new(VecModel::from(page_rows)).into());
        }
        app.set_streams_hint(SharedString::from(hint));
    }

    /// Move the visible stream list to an absolute (zero-based) page.
    pub(super) fn stream_page_picked(&self, page: usize) {
        {
            let mut state = self.shared.lock().unwrap();
            state.stream_page = page;
        }
        self.apply_stream_filter();
    }

    /// A filter pill was tapped (0 = All, else 1-based addon index).
    pub(super) fn stream_filter_picked(&self, index: usize) {
        {
            let mut state = self.shared.lock().unwrap();
            if index <= state.stream_addons.len() {
                state.stream_filter = index;
                state.stream_page = 0;
            } else {
                return;
            }
        }
        self.apply_stream_filter();
        if let Some(app) = self.app() {
            app.set_stream_filter(index as i32);
            app.set_detail_kb_s(0);
        }
    }

    /// Clear the stream list and its addon-filter state (state + UI).
    pub(super) fn clear_streams(&self) {
        {
            let mut state = self.shared.lock().unwrap();
            state.stream_generation = state.stream_generation.wrapping_add(1);
            state.streams.clear();
            state.stream_all.clear();
            state.stream_addons.clear();
            state.stream_pending.clear();
            state.stream_filter = 0;
            state.stream_page = 0;
        }
        if let Some(app) = self.app() {
            app.set_streams(Rc::new(VecModel::<StreamRow>::from(vec![])).into());
            app.set_stream_addons(Rc::new(VecModel::<SharedString>::from(vec![])).into());
            app.set_stream_loading(Rc::new(VecModel::<bool>::from(vec![])).into());
            app.set_streams_searching(false);
            app.set_stream_filter(0);
            app.set_stream_page(0);
            app.set_stream_page_count(1);
            app.set_stream_page_start(0);
            app.set_stream_total(0);
            app.set_stream_action_open(false);
        }
    }

    pub(super) fn stream_picked(&self, index: usize) {
        let stream = {
            let state = self.shared.lock().unwrap();
            state.streams.get(index).cloned()
        };
        let stream = match stream {
            Some(s) => s,
            None => return,
        };
        if let Some(app) = self.app() {
            app.set_detail_kb_s(index as i32);
        }

        match stream.source.clone() {
            StreamSource::Url(url) => {
                // A direct stream supersedes any torrent the tick may still be
                // reporting stats for. Stop it first so a no-cache torrent's
                // data is actually deleted instead of being left behind.
                {
                    if let Some(engine) = crate::torrent::engine() {
                        engine.on_playback_stopped(&active_torrent_settings());
                    }
                    self.shared.lock().unwrap().active_torrent = None;
                }
                // Player backend (Settings → Player): external hands the URL to
                // another video app unconditionally; otherwise in-app playback
                // first (mpv on desktop and Android, HTML5 video on web), with
                // the external app only as a fallback when the player cannot
                // take the stream at all.
                if nova_config::active_cache_settings().player_external {
                    let _ = crate::player::open_external(&url);
                } else if !self.open_player(url.clone()) {
                    // Detached launch; the external app manages its own process.
                    let _ = crate::player::open_external(&url);
                }
            }
            StreamSource::UrlWithOptions {
                url,
                headers,
                subtitles,
            } => {
                if let Some(engine) = crate::torrent::engine() {
                    engine.on_playback_stopped(&active_torrent_settings());
                }
                self.shared.lock().unwrap().active_torrent = None;
                // Keep source headers and subtitle tracks together in mpv.
                if !self.open_player_with_options(url, headers, subtitles) {
                    self.set_stream_hint(Some(StreamHint::Fixed(
                        "This stream requires the in-app player, which is unavailable.",
                    )));
                }
            }
            StreamSource::Torrent {
                info_hash,
                file_idx,
            } => {
                self.play_torrent(&stream.display, info_hash, file_idx);
            }
            StreamSource::Unsupported => {
                self.set_stream_hint(Some(StreamHint::Fixed(
                    "This stream cannot be played here.",
                )));
            }
            StreamSource::Downloaded { job_id, path } => {
                let job = self.downloads.job(&job_id);
                if job.is_some_and(|job| job.phase == crate::download::DownloadPhase::Completed)
                    && path.is_file()
                {
                    let _ = self.open_player(path.to_string_lossy().into_owned());
                } else {
                    self.set_stream_hint(Some(StreamHint::Fixed(
                        "This download is not complete yet.",
                    )));
                }
            }
        }
    }

    /// Set a one-off detail hint in semantic form so it can be rerendered after
    /// a language change. Stream-list-derived hints are set by
    /// `apply_stream_filter` instead.
    pub(super) fn set_stream_hint(&self, hint: Option<StreamHint>) {
        let rendered = hint.as_ref().map(StreamHint::render).unwrap_or_default();
        self.shared.lock().unwrap().stream_hint = hint;
        if let Some(app) = self.app() {
            app.set_streams_hint(SharedString::from(rendered));
        }
    }

    /// Refresh localized labels in an open detail modal while preserving its
    /// decoded artwork and current list delegates wherever the row shape is
    /// unchanged.
    pub(super) fn refresh_detail_language_text(&self) {
        let Some(app) = self.app() else { return };
        if !app.get_modal_visible() {
            return;
        }

        let (seasons, episode_page) = {
            let state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_ref() else {
                return;
            };
            (modal.seasons.clone(), modal.episode_page)
        };
        app.set_season_names(
            Rc::new(VecModel::from(
                seasons
                    .iter()
                    .map(|&season| SharedString::from(season_label(season)))
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );

        let season_cards = self.current_season_cards();
        let season_model = app.get_season_cards();
        if season_model.row_count() == season_cards.len() {
            for (index, fresh) in season_cards.into_iter().enumerate() {
                if let Some(mut current) = season_model.row_data(index)
                    && current.name != fresh.name
                {
                    current.name = fresh.name;
                    season_model.set_row_data(index, current);
                }
            }
        } else {
            app.set_season_cards(Rc::new(VecModel::from(season_cards)).into());
        }

        let page_rows: Vec<EpisodeRow> = self
            .current_episode_rows()
            .into_iter()
            .skip(episode_page * EPISODE_PAGE_SIZE)
            .take(EPISODE_PAGE_SIZE)
            .collect();
        let episode_model = app.get_episode_rows();
        if episode_model.row_count() == page_rows.len() {
            for (index, fresh) in page_rows.into_iter().enumerate() {
                if let Some(mut current) = episode_model.row_data(index) {
                    let changed = current.text != fresh.text
                        || current.details != fresh.details
                        || current.lines != fresh.lines
                        || current.date != fresh.date;
                    if changed {
                        current.text = fresh.text;
                        current.details = fresh.details;
                        current.lines = fresh.lines;
                        current.date = fresh.date;
                        episode_model.set_row_data(index, current);
                    }
                }
            }
        } else {
            self.apply_episode_rows();
        }

        let (one_off_hint, has_stream_model) = {
            let state = self.shared.lock().unwrap();
            (
                state.stream_hint.clone(),
                !state.stream_pending.is_empty()
                    || !state.stream_all.is_empty()
                    || !state.streams.is_empty(),
            )
        };
        if one_off_hint.is_some() || has_stream_model || !app.get_streams_hint().is_empty() {
            self.apply_stream_filter();
            if let Some(hint) = one_off_hint {
                self.set_stream_hint(Some(hint));
            }
        }
        if app.get_stream_action_open() {
            self.open_stream_action(app.get_stream_action_id().as_str());
        }
    }
}

fn stream_response_is_current(state: &Shared, request_id: &str, generation: u64) -> bool {
    state.stream_generation == generation
        && state
            .modal_item
            .as_ref()
            .is_some_and(|modal| modal.request_id == request_id)
}

fn stream_endpoint(
    addon: &Installed,
    media_type: &str,
    request_id: &str,
    lookup: Option<&nova_providers::StreamLookupRequest>,
    stream_ids: &[String],
) -> Option<String> {
    if !addon.enabled || !addon.manifest.has_streams() {
        return None;
    }
    // Contextual resolution is explicit for the bundled source. Its own
    // manifest restrictions continue to protect normal meta/stream routing.
    if addon.url == nova_providers::ANIKOTO_PROVIDER_URL && !request_id.starts_with("anikoto:") {
        return nova_providers::builtin_stream_lookup_url(lookup?).ok();
    }
    let target_id = if addon.url != nova_providers::ANIKOTO_PROVIDER_URL
        && request_id.starts_with("anikoto:")
    {
        // A source episode keeps its own library identity. Only the outbound
        // addon request uses a canonical episode ID confirmed by the mapper.
        stream_ids
            .iter()
            .find(|id| addon.manifest.accepts("stream", media_type, id))
            .map(String::as_str)?
    } else {
        request_id
    };
    if !addon.manifest.accepts("stream", media_type, target_id) {
        return None;
    }
    Some(
        Addon::new(&addon.url)
            .ok()?
            .stream_url(media_type, target_id),
    )
}

fn episode_stream_ids(modal: &ModalItem, request_id: &str) -> Vec<String> {
    if !modal.id.starts_with("anikoto:") || !request_id.starts_with("anikoto:") {
        return Vec::new();
    }
    modal
        .videos
        .iter()
        .find(|video| video.id == request_id)
        .and_then(|video| video.extra.get("novaStreamIds"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .take(16)
        .filter_map(serde_json::Value::as_str)
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 1024
                && !id.starts_with("anikoto:")
                && !id.chars().any(char::is_control)
        })
        .map(String::from)
        .collect()
}

fn merge_episode_stream_ids(existing: &mut [Video], fresh: &[Video]) {
    let fresh = fresh
        .iter()
        .map(|video| (video.id.as_str(), video))
        .collect::<std::collections::HashMap<_, _>>();
    for video in existing
        .iter_mut()
        .filter(|video| video.id.starts_with("anikoto:"))
    {
        let Some(updated) = fresh.get(video.id.as_str()) else {
            continue;
        };
        if let Some(ids) = updated.extra.get("novaStreamIds") {
            video.extra.insert("novaStreamIds".into(), ids.clone());
        } else {
            video.extra.remove("novaStreamIds");
        }
    }
}

fn source_stream_lookup(
    modal: &ModalItem,
    request_id: &str,
) -> Option<nova_providers::StreamLookupRequest> {
    if modal.type_ != "series" || modal.id.starts_with("anikoto:") {
        return None;
    }
    let video = modal.videos.iter().find(|video| video.id == request_id)?;
    let season = video.season.filter(|season| (1..=100).contains(season))?;
    let episode = video
        .episode
        .or(video.number)
        .filter(|number| *number > 0)?;
    // Addons disagree about season boundaries. Supply the verified metadata
    // sequence as well as the local number so sources can align split cours.
    // A missing season/episode must not silently shift every later episode.
    let mut season_numbers =
        std::collections::BTreeMap::<u32, std::collections::BTreeSet<u32>>::new();
    let mut invalid_seasons = std::collections::BTreeSet::new();
    for video in &modal.videos {
        let Some(season) = video.season.filter(|season| *season > 0) else {
            continue;
        };
        let numbers = season_numbers.entry(season).or_default();
        if let Some(number) = video.episode_number().filter(|number| *number > 0) {
            numbers.insert(number);
        } else {
            invalid_seasons.insert(season);
        }
    }
    let season_counts = season_numbers
        .iter()
        .filter_map(|(&season, numbers)| {
            let maximum = numbers.last().copied()?;
            (maximum <= 10_000
                && numbers.len() == maximum as usize
                && !invalid_seasons.contains(&season))
            .then_some((season, maximum))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let absolute_episode = (1..season).try_fold(episode, |total, previous| {
        total.checked_add(*season_counts.get(&previous)?)
    });
    let last_season = season_numbers.keys().next_back().copied()?;
    let series_episode_count =
        if last_season <= 100 && modal.videos.iter().all(|video| video.season.is_some()) {
            (1..=last_season).try_fold(0_u32, |total, season| {
                total.checked_add(*season_counts.get(&season)?)
            })
        } else {
            None
        };
    Some(nova_providers::StreamLookupRequest {
        media_id: modal.id.clone(),
        media_type: modal.type_.clone(),
        title: modal.name.clone(),
        year: (!modal.year.is_empty()).then(|| modal.year.clone()),
        season,
        episode,
        absolute_episode,
        season_episode_count: season_counts.get(&season).copied(),
        series_episode_count,
        episode_title: (!video.label().is_empty()).then(|| video.label()),
        released: video.released.clone(),
    })
}

#[cfg(test)]
mod source_lookup_tests {
    use super::*;

    fn modal() -> ModalItem {
        let mut videos = (1..=25)
            .map(|number| Video {
                id: format!("tt0994314:1:{number}"),
                season: Some(1),
                episode: Some(number),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        videos.push(Video {
            id: "foreign-opaque-episode".into(),
            name: "The Day a Demon Awakens".into(),
            season: Some(2),
            episode: Some(1),
            released: Some("2008-04-06".into()),
            ..Default::default()
        });
        ModalItem {
            open_token: Arc::new(()),
            pending_watch_now: None,
            episodes_loading: false,
            id: "tt0994314".into(),
            type_: "series".into(),
            request_id: "foreign-opaque-episode".into(),
            videos,
            seasons: vec![1, 2],
            season_index: 1,
            episode_page: 0,
            name: "Code Geass".into(),
            year: "2006–2008".into(),
            poster_url: String::new(),
            background_url: String::new(),
            description: String::new(),
            genres: Vec::new(),
        }
    }

    #[test]
    fn foreign_library_episode_uses_contextual_source_route_and_keeps_original_ids() {
        let modal = modal();
        let lookup = source_stream_lookup(&modal, &modal.request_id).unwrap();
        assert_eq!(lookup.media_id, "tt0994314");
        assert_eq!(lookup.season, 2);
        assert_eq!(lookup.episode, 1);
        assert_eq!(lookup.absolute_episode, Some(26));
        assert_eq!(lookup.season_episode_count, Some(1));
        assert_eq!(lookup.series_episode_count, Some(26));
        let mut addon = Installed {
            url: nova_providers::ANIKOTO_PROVIDER_URL.into(),
            label: "AniKoto".into(),
            enabled: true,
            configure_ok: Some(false),
            manifest: nova_providers::builtin_manifest(),
            available: true,
            generation: 1,
        };
        assert!(
            !addon
                .manifest
                .accepts("stream", "series", &modal.request_id)
        );
        let url = stream_endpoint(&addon, "series", &modal.request_id, Some(&lookup), &[]).unwrap();
        assert!(url.starts_with("nova-provider://anikoto/resolve/series/"));
        assert_eq!(modal.id, "tt0994314");
        assert_eq!(modal.request_id, "foreign-opaque-episode");
        assert!(stream_endpoint(&addon, "series", &modal.request_id, None, &[]).is_none());
        addon.enabled = false;
        assert!(stream_endpoint(&addon, "series", &modal.request_id, Some(&lookup), &[]).is_none());
        addon.enabled = true;
        addon.url = "https://example.com".into();
        addon.manifest.id_prefixes.clear();
        let url = stream_endpoint(&addon, "series", &modal.request_id, Some(&lookup), &[]).unwrap();
        assert!(url.ends_with("/stream/series/foreign-opaque-episode.json"));
    }

    #[test]
    fn native_source_episodes_use_confirmed_aliases_for_compatible_addons() {
        let mut modal = modal();
        modal.id = "anikoto:asterisk-season-2".into();
        modal.request_id = "anikoto:ep:native-episode-1".into();
        modal.videos = vec![Video {
            id: modal.request_id.clone(),
            season: Some(1),
            episode: Some(1),
            extra: std::collections::HashMap::from([(
                "novaStreamIds".into(),
                serde_json::json!(["kitsu:unsupported", "tt5095466:1:13"]),
            )]),
            ..Default::default()
        }];
        let ids = episode_stream_ids(&modal, &modal.request_id);
        let mut addon = Installed {
            url: "https://example.com/configured-addon".into(), label: "Streams".into(),
            enabled: true, configure_ok: Some(false), available: true, generation: 1,
            manifest: serde_json::from_value(serde_json::json!({"id":"streams", "name":"Streams", "version":"1", "types":["series"], "resources":["stream"], "idPrefixes":["tt"]})).unwrap(),
        };
        let url = stream_endpoint(&addon, "series", &modal.request_id, None, &ids).unwrap();
        assert!(url.ends_with("/stream/series/tt5095466:1:13.json"));
        assert!(stream_endpoint(&addon, "series", &modal.request_id, None, &[]).is_none());
        addon.enabled = false;
        assert!(stream_endpoint(&addon, "series", &modal.request_id, None, &ids).is_none());
        addon.enabled = true;
        addon.url = nova_providers::ANIKOTO_PROVIDER_URL.into();
        addon.manifest = nova_providers::builtin_manifest();
        let url = stream_endpoint(&addon, "series", &modal.request_id, None, &ids).unwrap();
        assert!(url.ends_with("/stream/series/anikoto:ep:native-episode-1.json"));
        assert_eq!(modal.videos[0].id, modal.request_id);
        let round_trip: Video =
            serde_json::from_slice(&serde_json::to_vec(&modal.videos[0]).unwrap()).unwrap();
        assert_eq!(
            round_trip.extra["novaStreamIds"],
            modal.videos[0].extra["novaStreamIds"]
        );
    }

    #[test]
    fn aiostreams_and_torrentio_receive_slime_season_four_ids_from_native_episode_one() {
        let mut modal = modal();
        modal.id = "anikoto:slime-s4".into();
        modal.request_id = "anikoto:ep:source-episode-one".into();
        modal.videos = vec![Video {
            id: modal.request_id.clone(),
            season: Some(1),
            episode: Some(1),
            extra: std::collections::HashMap::from([(
                "novaStreamIds".into(),
                serde_json::json!(["tt9054364:4:1"]),
            )]),
            ..Default::default()
        }];
        let ids = episode_stream_ids(&modal, &modal.request_id);
        for (name, url, prefixes) in [
            (
                "Torrentio",
                "https://torrentio.strem.fun/providers=nyaasi",
                vec!["tt", "kitsu"],
            ),
            (
                "AIOStreams",
                "https://aiostreams.example/configured-user/configured-addon",
                vec!["tt", "kitsu:", "tmdb:"],
            ),
        ] {
            let addon = Installed { url: url.into(), label: name.into(), enabled: true,
                configure_ok: Some(false), available: true, generation: 1,
                manifest: serde_json::from_value(serde_json::json!({"id":name, "name":name, "version":"1", "types":["movie","series","anime"],
                    "resources":[{"name":"stream","types":["movie","series","anime"],"idPrefixes":prefixes}]})).unwrap() };
            assert_eq!(
                stream_endpoint(&addon, "series", &modal.request_id, None, &ids),
                Some(format!("{url}/stream/series/tt9054364:4:1.json"))
            );
            assert!(stream_endpoint(&addon, "series", &modal.request_id, None, &[]).is_none());
            let mut unrestricted = addon;
            unrestricted.manifest.resources = vec![addons::Resource::Plain("stream".into())];
            unrestricted.manifest.id_prefixes.clear();
            assert!(
                stream_endpoint(&unrestricted, "series", &modal.request_id, None, &[]).is_none(),
                "unrestricted manifests must not get an unresolved private source ID"
            );
        }
        assert_eq!(modal.request_id, "anikoto:ep:source-episode-one");
        assert_eq!(modal.videos[0].season, Some(1));
    }

    #[test]
    fn old_replies_cannot_remove_addon_pills_after_aliases_restart_the_same_episode() {
        let mut modal = modal();
        modal.id = "anikoto:slime-s4".into();
        modal.request_id = "anikoto:ep:episode-one".into();
        let state = Shared {
            modal_item: Some(modal),
            stream_generation: 3,
            ..Default::default()
        };
        assert!(stream_response_is_current(
            &state,
            "anikoto:ep:episode-one",
            3
        ));
        assert!(!stream_response_is_current(
            &state,
            "anikoto:ep:episode-one",
            2
        ));
        assert!(!stream_response_is_current(
            &state,
            "anikoto:ep:episode-two",
            3
        ));
    }

    #[test]
    fn fresh_aliases_update_cached_episodes_without_changing_picker_identity() {
        let mut video = Video {
            id: "anikoto:ep:source".into(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        };
        let mut fresh = video.clone();
        fresh.extra.insert(
            "novaStreamIds".into(),
            serde_json::json!(["tt5095466:1:13"]),
        );
        merge_episode_stream_ids(
            std::slice::from_mut(&mut video),
            std::slice::from_ref(&fresh),
        );
        assert_eq!(video.extra["novaStreamIds"], fresh.extra["novaStreamIds"]);
        assert_eq!(video.id, "anikoto:ep:source");
        assert_eq!(video.episode, Some(1));
        fresh.extra.clear();
        merge_episode_stream_ids(std::slice::from_mut(&mut video), &[fresh]);
        assert!(!video.extra.contains_key("novaStreamIds"));
    }

    #[test]
    fn incomplete_seasons_do_not_guess_absolute_numbers_or_specials() {
        let mut modal = modal();
        modal.videos.remove(0);
        assert_eq!(
            source_stream_lookup(&modal, &modal.request_id)
                .unwrap()
                .absolute_episode,
            None
        );
        assert!(source_stream_lookup(&modal, "missing").is_none());
        modal.videos.last_mut().unwrap().season = Some(0);
        assert!(source_stream_lookup(&modal, &modal.request_id).is_none());
        modal.id = "anikoto:source".into();
        assert!(source_stream_lookup(&modal, &modal.request_id).is_none());
    }

    #[test]
    fn source_lookup_supplies_merged_season_counts_and_excludes_specials() {
        let mut modal = modal();
        modal.name = "The Asterisk War".into();
        modal.videos.truncate(24);
        modal.request_id = modal.videos[12].id.clone();
        modal.videos.push(Video {
            id: "special".into(),
            season: Some(0),
            episode: Some(1),
            ..Default::default()
        });
        let lookup = source_stream_lookup(&modal, &modal.request_id).unwrap();
        assert_eq!(lookup.absolute_episode, Some(13));
        assert_eq!(lookup.season_episode_count, Some(24));
        assert_eq!(lookup.series_episode_count, Some(24));
        // Missing numbering or seasons cannot establish a series total.
        modal.videos[3].episode = None;
        let lookup = source_stream_lookup(&modal, &modal.request_id).unwrap();
        assert_eq!(lookup.season_episode_count, None);
        assert_eq!(lookup.series_episode_count, None);
        modal.videos[3].season = Some(u32::MAX);
        assert_eq!(
            source_stream_lookup(&modal, &modal.request_id)
                .unwrap()
                .series_episode_count,
            None
        );
    }
}

/// Stable-sort addon stream rows into installed-addon order (unknown addons
/// last). Arrival order within an addon is preserved, so results appearing
/// addon-by-addon stay grouped as they stream in.
fn sort_streams_by_addon(rows: &mut [StreamUi], addon_order: &[String]) {
    rows.sort_by_key(|s| {
        addon_order
            .iter()
            .position(|a| a == &s.addon)
            .unwrap_or(usize::MAX)
    });
}

/// Whether the currently pushed stream-row model has the same shape as `rows`
/// (same length and same row ids in the same order). When true the rows can be
/// updated in place — preserving their Slint delegates and the Android
/// hold-to-open action-sheet timer — instead of replacing the whole model.
pub(crate) fn stream_rows_same_shape<M: Model<Data = StreamRow>>(
    existing: &M,
    rows: &[StreamRow],
) -> bool {
    existing.row_count() == rows.len()
        && rows.iter().enumerate().all(|(index, row)| {
            existing
                .row_data(index)
                .is_some_and(|previous| previous.id == row.id)
        })
}

/// Directory holding cached episode lists (keyed by meta type + item id).
#[allow(dead_code)]
#[cfg(feature = "desktop")]
pub(crate) fn episodes_cache_dir() -> PathBuf {
    app_cache_dir().join("episodes")
}
/// Cache file name for one item's episode list.
#[cfg(feature = "desktop")]
#[allow(dead_code)]
pub(crate) fn episodes_cache_path_in(dir: &Path, type_: &str, id: &str) -> PathBuf {
    let key = format!("{type_}\u{1}{id}");
    dir.join(format!("{:016x}.json", fnv1a(key.as_bytes())))
}
/// Core: read a cached episode list (None when absent/unreadable/corrupt).
#[cfg(feature = "desktop")]
#[allow(dead_code)]
pub(crate) fn read_cached_episodes(dir: &Path, type_: &str, id: &str) -> Option<Vec<Video>> {
    let text = fs::read_to_string(episodes_cache_path_in(dir, type_, id)).ok()?;
    serde_json::from_str(&text).ok()
}
/// Core: persist an episode list.
#[cfg(feature = "desktop")]
#[allow(dead_code)]
pub(crate) fn write_cached_episodes(dir: &Path, type_: &str, id: &str, videos: &[Video]) {
    if fs::create_dir_all(dir).is_err() {
        eprintln!("nova: could not create episodes cache dir {:?}", dir);
        return;
    }
    let contents = match serde_json::to_string_pretty(videos) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("nova: could not serialise episode cache: {e}");
            return;
        }
    };
    if let Err(e) = atomic_write(&episodes_cache_path_in(dir, type_, id), &contents) {
        eprintln!("nova: could not write episode cache: {e}");
    }
}
/// App-level wrappers over the episode cache in the KV store.
pub(crate) fn episodes_key(type_: &str, id: &str) -> String {
    format!("episodes:{type_}\u{1}{id}")
}
pub(crate) fn read_episodes_cache_for(type_: &str, id: &str) -> Option<Vec<Video>> {
    read_json::<Vec<Video>>(&episodes_key(type_, id))
}
pub(crate) fn write_episodes_cache_for(type_: &str, id: &str, videos: &[Video]) {
    write_json(&episodes_key(type_, id), videos);
}
pub(crate) fn meta_header_key(type_: &str, id: &str) -> String {
    format!("meta_header:{type_}\u{1}{id}")
}
pub(crate) fn read_meta_header_for(type_: &str, id: &str) -> Option<MetaHeader> {
    read_json::<MetaHeader>(&meta_header_key(type_, id))
}
pub(crate) fn write_meta_header_for(type_: &str, id: &str, header: &MetaHeader) {
    write_json(&meta_header_key(type_, id), header);
}
/// Build a header snapshot from a fetched meta item (empty when the addon
/// sent nothing usable).
pub(crate) fn meta_header_from_item(item: &MetaItem) -> MetaHeader {
    let mut preview = item.preview.clone();
    preview.extra.extend(item.extra.clone());
    tracking::remember_source(&preview);
    MetaHeader {
        background_url: item.preview.background.clone().unwrap_or_default(),
        description: item.preview.description.clone().unwrap_or_default(),
        genres: item.preview.genres.clone(),
        year: item.preview.year_str().unwrap_or_default(),
    }
}
/// Whether the header cache holds usable text (description or genres) for
/// `(type_, id)`. Used to decide prefetch skips: episodes alone aren't
/// enough, or pills + synopsis would still need a network round-trip.
pub(crate) fn header_text_cached(type_: &str, id: &str) -> bool {
    read_meta_header_for(type_, id)
        .map(|h| !h.description.is_empty() || !h.genres.is_empty())
        .unwrap_or(false)
}
/// Merge `fresh` into the cached header for `(type_, id)`, filling empty
/// slots only. Returns true when the stored value changed (or was created
/// with non-empty content).
pub(crate) fn merge_meta_header_for(type_: &str, id: &str, fresh: &MetaHeader) -> bool {
    if fresh.background_url.is_empty()
        && fresh.description.is_empty()
        && fresh.genres.is_empty()
        && fresh.year.is_empty()
    {
        return false;
    }
    let mut cached = read_meta_header_for(type_, id).unwrap_or_default();
    let mut touched = false;
    if cached.background_url.is_empty() && !fresh.background_url.is_empty() {
        cached.background_url = fresh.background_url.clone();
        touched = true;
    }
    if cached.description.is_empty() && !fresh.description.is_empty() {
        cached.description = fresh.description.clone();
        touched = true;
    }
    if cached.genres.is_empty() && !fresh.genres.is_empty() {
        cached.genres.clone_from(&fresh.genres);
        touched = true;
    }
    if cached.year.is_empty() && !fresh.year.is_empty() {
        cached.year.clone_from(&fresh.year);
        touched = true;
    }
    if touched {
        write_meta_header_for(type_, id, &cached);
    }
    touched
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(addon: &str, id: &str) -> StreamUi {
        StreamUi {
            id: id.into(),
            display: id.into(),
            source: StreamSource::Unsupported,
            addon: addon.into(),
            download: None,
        }
    }

    #[test]
    fn streams_sort_into_installed_addon_order_keeping_arrival_order() {
        let order = vec!["B".to_string(), "A".to_string()];
        let mut rows = vec![
            row("A", "a1"),
            row("C", "c1"),
            row("B", "b1"),
            row("A", "a2"),
        ];
        sort_streams_by_addon(&mut rows, &order);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        // Installed order (B, then A), unknown addon (C) last, arrival order
        // within an addon preserved.
        assert_eq!(ids, vec!["b1", "a1", "a2", "c1"]);
    }
}

//! Poster / backdrop / episode-thumbnail image pipeline.
use super::*;

impl Bridge {
    /// Search results have their own generation and model, so their posters
    /// use the shared image decoder but never the browse-grid poster queue.
    pub(super) fn fetch_search_card_poster(
        &self,
        type_: String,
        id: String,
        url: String,
        generation: u64,
    ) {
        {
            let mut state = self.shared.lock().unwrap();
            if state.search_generation != generation
                || !state
                    .search_poster_inflight
                    .insert((generation, type_.clone(), id.clone()))
            {
                return;
            }
        }

        let app_weak = self.app.clone();
        let bridge = self.clone();
        net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
            let _ = slint::invoke_from_event_loop(move || {
                let index = {
                    let mut state = bridge.shared.lock().unwrap();
                    state
                        .search_poster_inflight
                        .remove(&(generation, type_.clone(), id.clone()));
                    if state.search_generation == generation {
                        state
                            .search_previews
                            .iter()
                            .position(|meta| meta.type_ == type_ && meta.id == id)
                    } else {
                        None
                    }
                };
                if let (Some(pixels), Some(index), Some(app)) = (pixels, index, app_weak.upgrade())
                {
                    app.invoke_set_search_card_poster(index as i32, Image::from_rgba8(pixels));
                }
            });
        });
    }

    /// (Re)fetch the poster for one card (scroll-back after unloading, or a
    /// not-yet-loaded row inside the ±2-row preload window). Near-viewport
    /// work goes on the high-priority channel so it jumps ahead of the
    /// background full-grid sweep.
    pub(super) fn fetch_card_poster(&self, library: bool, index: usize, url: String) {
        #[cfg(feature = "desktop")]
        {
            let generation = self.catalog_gen.load(Ordering::Relaxed);
            self.poster_tx.send_hi(PosterJob {
                generation,
                index,
                url,
                library,
            });
        }
        #[cfg(not(feature = "desktop"))]
        {
            let Some(app) = self.app() else {
                return;
            };
            let app_weak = app.as_weak();
            let gen_counter = self.catalog_gen.clone();
            let generation = gen_counter.load(Ordering::Relaxed);
            net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
                let Some(pixels) = pixels else {
                    return;
                };
                let _ = slint::invoke_from_event_loop(move || {
                    if generation != gen_counter.load(Ordering::Relaxed) {
                        return;
                    }
                    if let Some(app) = app_weak.upgrade() {
                        let img = Image::from_rgba8(pixels);
                        if library {
                            app.invoke_set_library_poster(index as i32, img.clone());
                        } else {
                            app.invoke_set_card_poster(index as i32, img.clone());
                        }
                    }
                });
            });
        }
    }

    /// Queue one thumbnail download per still-missing episode of the current
    /// season; fetching is throttled by `pump_episode_thumbs`.
    pub(super) fn queue_episode_thumbnails(&self) {
        self.queue_episode_thumbs_inner(false);
    }

    /// Queue a refresh check per already-cached thumbnail of the current
    /// season (see `EpisodeThumbJob::verify`): fresh bytes download in the
    /// background and rows repaint only when art actually changed. Missing
    /// thumbnails are NOT queued here — the normal pass above handles those.
    pub(super) fn queue_episode_thumbnail_verify(&self) {
        self.queue_episode_thumbs_inner(true);
    }

    /// Shared episode-thumbnail queue builder. The normal pass (`verify ==
    /// false`) fetches the still-missing episodes; the verify pass re-checks
    /// the already-cached ones. Both respect the episode filter and the
    /// `show_unwatched_thumbs` setting identically, and both sets stay
    /// disjoint through the shared `EPISODE_INFLIGHT` guard.
    pub(super) fn queue_episode_thumbs_inner(&self, verify: bool) {
        let jobs: Vec<EpisodeThumbJob> = {
            let state = self.shared.lock().unwrap();
            let Some(m) = state.modal_item.as_ref() else {
                return;
            };
            if m.seasons.is_empty() {
                return;
            }
            let season = m.seasons[m.season_index.min(m.seasons.len() - 1)];
            let show_unwatched = state.cache_settings.show_unwatched_thumbs;
            let mut inflight = EPISODE_INFLIGHT.lock().unwrap();
            let mut jobs = Vec::new();
            // Only the filtered (visible) episodes need thumbnails.
            let filter = self
                .app()
                .map(|a| a.get_episode_filter().to_string())
                .unwrap_or_default();
            for v in season_episodes(&m.videos, season) {
                if !episode_matches_filter(v, &filter) {
                    continue;
                }
                // Hidden by the `show_unwatched_thumbs` setting: don't
                // download what the rows won't display.
                if !show_unwatched {
                    let (watched, progress, _) =
                        Self::episode_watch_state(&state.progress, &m.id, &v.id);
                    if !Self::episode_thumb_visible(show_unwatched, watched, progress) {
                        continue;
                    }
                }
                if let Some(url) = &v.thumbnail
                    && {
                        let cached =
                            decoded_cache_contains(&sized_cache_key(url, Some(EPISODE_THUMB_SIDE)));
                        // Normal pass takes the missing ones, verify pass
                        // the cached ones — disjoint by construction.
                        cached == verify
                    }
                    && inflight.insert(url.clone())
                {
                    jobs.push(EpisodeThumbJob {
                        url: url.clone(),
                        item_id: m.id.clone(),
                        season_index: m.season_index,
                        verify,
                    });
                }
            }
            jobs
        };
        if jobs.is_empty() {
            return;
        }
        {
            let mut queue = EPISODE_QUEUE.lock().unwrap();
            for job in jobs {
                queue.push_back(job);
            }
        }
        Self::pump_episode_thumbs(self.clone());
    }

    /// Worker pump: keep at most a few thumbnail downloads running at once.
    /// Completed downloads are picked up by a debounced re-render of the list
    /// (never a rebuild per image), so seasons with hundreds of episodes stay
    /// responsive while switching.
    pub(super) fn pump_episode_thumbs(bridge: Bridge) {
        const MAX_ACTIVE: usize = 4;
        loop {
            if EPISODE_ACTIVE.load(Ordering::SeqCst) >= MAX_ACTIVE {
                break;
            }
            let job = EPISODE_QUEUE.lock().unwrap().pop_front();
            let Some(job) = job else {
                break;
            };
            EPISODE_ACTIVE.fetch_add(1, Ordering::SeqCst);
            if job.verify {
                // Refresh check: the cached art stays painted while fresh
                // bytes download; rows repaint only on a real pixel change.
                let worker = bridge.clone();
                refresh_image_if_changed(job.url.clone(), Some(EPISODE_THUMB_SIDE), move |fresh| {
                    EPISODE_INFLIGHT.lock().unwrap().remove(&job.url);
                    EPISODE_ACTIVE.fetch_sub(1, Ordering::SeqCst);
                    // Kick off the next queued download.
                    Bridge::pump_episode_thumbs(worker.clone());
                    if fresh.is_none() {
                        return; // unchanged or failed: cached art is shown
                    }
                    Bridge::schedule_episode_thumb_render(worker, job.item_id, job.season_index);
                });
                continue;
            }
            let worker = bridge.clone();
            net::fetch_image(job.url.clone(), Some(EPISODE_THUMB_SIDE), move |pixels| {
                let loaded = pixels.is_some();
                EPISODE_INFLIGHT.lock().unwrap().remove(&job.url);
                EPISODE_ACTIVE.fetch_sub(1, Ordering::SeqCst);
                // Kick off the next queued download.
                Bridge::pump_episode_thumbs(worker.clone());
                if !loaded {
                    return; // keep the placeholder
                }
                Bridge::schedule_episode_thumb_render(worker, job.item_id, job.season_index);
            });
        }
    }

    /// Debounced episode-list re-render after thumbnail downloads: at most
    /// one render is scheduled; it picks up every thumbnail finished so far
    /// and only fires while the same item + season list is still showing.
    pub(super) fn schedule_episode_thumb_render(
        bridge: Bridge,
        item_id: String,
        season_index: usize,
    ) {
        if !EPISODE_RENDER_PENDING.swap(true, Ordering::SeqCst) {
            let _ = slint::invoke_from_event_loop(move || {
                let timer = slint::Timer::default();
                timer.start(
                    slint::TimerMode::SingleShot,
                    Duration::from_millis(120),
                    move || {
                        EPISODE_RENDER_PENDING.store(false, Ordering::SeqCst);
                        let still_on_list = {
                            let state = bridge.shared.lock().unwrap();
                            state
                                .modal_item
                                .as_ref()
                                .map(|m| m.id == item_id && m.season_index == season_index)
                                .unwrap_or(false)
                        };
                        if still_on_list
                            && bridge
                                .app()
                                .map(|a| a.get_modal_episodes())
                                .unwrap_or(false)
                        {
                            bridge.apply_episode_rows();
                        }
                    },
                );
                std::mem::forget(timer);
            });
        }
    }

    /// Fetch still-missing season card artwork, then repaint the cards.
    /// Seasons are few (unlike episodes), so each missing thumb downloads
    /// directly instead of joining the capped episode pool; completion is
    /// guarded on the same item + season list.
    pub(super) fn dispatch_season_thumbs(&self) {
        let (item_id, seasons, urls): (String, Vec<u32>, Vec<String>) = {
            let state = self.shared.lock().unwrap();
            let Some(m) = state.modal_item.as_ref() else {
                return;
            };
            let mut urls = Vec::new();
            for &s in &m.seasons {
                if let Some(url) = season_thumb_url(&m.videos, s)
                    && !decoded_cache_contains(&sized_cache_key(&url, Some(EPISODE_THUMB_SIDE)))
                    && !urls.contains(&url)
                {
                    urls.push(url);
                }
            }
            (m.id.clone(), m.seasons.clone(), urls)
        };
        for url in urls {
            let bridge = self.clone();
            let item_id = item_id.clone();
            let seasons = seasons.clone();
            net::fetch_image(url, Some(EPISODE_THUMB_SIDE), move |pixels| {
                if pixels.is_none() {
                    return; // keep the placeholder
                }
                let _ = slint::invoke_from_event_loop(move || {
                    let same = bridge
                        .shared
                        .lock()
                        .unwrap()
                        .modal_item
                        .as_ref()
                        .map(|m| m.id == item_id && m.seasons == seasons)
                        .unwrap_or(false);
                    if same {
                        bridge.apply_season_cards();
                    }
                });
            });
        }
    }

    /// Load posters for the library grid off the UI thread: cached entries
    /// decode quickly, misses are downloaded once and file-cached. Updates
    /// are dropped if the card changed/vanished while the fetch ran.
    pub(super) fn dispatch_library_posters(&self, entries: &[LibraryEntry]) {
        let app_weak = self.app.clone();
        for (index, entry) in entries.iter().enumerate() {
            if entry.poster_url.is_empty() {
                continue;
            }
            let url = entry.poster_url.clone();
            let id = entry.id.clone();
            let weak = app_weak.clone();
            net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
                let Some(pixels) = pixels else {
                    return; // keep the placeholder
                };
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(app) = weak.upgrade() else {
                        return;
                    };
                    let model = app.get_library();
                    // Guard: card still present, still this item and not
                    // already filled by another race.
                    let matches = model
                        .row_data(index)
                        .map(|card: MediaCard| !card.is_loaded && card.id == id.as_str());
                    if matches == Some(true) {
                        app.invoke_set_library_poster(index as i32, Image::from_rgba8(pixels));
                    }
                });
            });
        }
    }

    /// Fetch still-missing Continue Watching posters off the UI thread.
    /// Completion is guarded on the row still showing this item unloaded.
    pub(super) fn dispatch_continue_posters(&self) {
        let items: Vec<(usize, String, String)> = {
            let state = self.shared.lock().unwrap();
            state
                .continue_list
                .iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    state
                        .entries
                        .iter()
                        .find(|e| e.id == c.series_id)
                        .filter(|e| !e.poster_url.is_empty())
                        .map(|e| (i, c.series_id.clone(), e.poster_url.clone()))
                })
                .collect()
        };
        let app_weak = self.app.clone();
        for (index, id, url) in items {
            let weak = app_weak.clone();
            net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
                let Some(pixels) = pixels else {
                    return; // keep the placeholder
                };
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(app) = weak.upgrade() else {
                        return;
                    };
                    let model = app.get_home_continue();
                    let matches = model
                        .row_data(index)
                        .map(|row: ContinueRow| !row.is_loaded && row.id == id.as_str());
                    if matches == Some(true) {
                        let Some(mut row) = model.row_data(index) else {
                            return;
                        };
                        row.poster = Image::from_rgba8(pixels);
                        row.is_loaded = true;
                        model.set_row_data(index, row);
                    }
                });
            });
        }
    }

    /// Fetch still-missing Upcoming posters off the UI thread (same guard
    /// pattern as [`Bridge::dispatch_continue_posters`]).
    pub(super) fn dispatch_upcoming_posters(&self) {
        let items: Vec<(usize, String, String)> = {
            let state = self.shared.lock().unwrap();
            state
                .upcoming_list
                .iter()
                .enumerate()
                .filter_map(|(i, u)| {
                    state
                        .entries
                        .iter()
                        .find(|e| e.id == u.series_id)
                        .filter(|e| !e.poster_url.is_empty())
                        .map(|e| (i, u.series_id.clone(), e.poster_url.clone()))
                })
                .collect()
        };
        let app_weak = self.app.clone();
        for (index, id, url) in items {
            let weak = app_weak.clone();
            net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
                let Some(pixels) = pixels else {
                    return; // keep the placeholder
                };
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(app) = weak.upgrade() else {
                        return;
                    };
                    let model = app.get_home_upcoming();
                    let matches = model
                        .row_data(index)
                        .map(|row: UpcomingRow| !row.is_loaded && row.id == id.as_str());
                    if matches == Some(true) {
                        let Some(mut row) = model.row_data(index) else {
                            return;
                        };
                        row.poster = Image::from_rgba8(pixels);
                        row.is_loaded = true;
                        model.set_row_data(index, row);
                    }
                });
            });
        }
    }

    /// Record a backdrop URL on a saved library entry (no-op when unchanged
    /// or not in library). Backfills entries saved before the backdrop URL
    /// was persisted, so later reopens can start the image load without
    /// waiting for a meta fetch.
    pub(super) fn persist_backdrop_for(&self, id: &str, url: &str) {
        if url.is_empty() {
            return;
        }
        let changed = {
            let mut state = self.shared.lock().unwrap();
            match state.entries.iter_mut().find(|e| e.id == id) {
                Some(e) if e.background_url != url => {
                    e.background_url = url.to_string();
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.persist_library();
        }
    }

    /// Record a poster URL on a saved library entry (no-op when unchanged,
    /// empty or not in library). Unlike header text this always overwrites:
    /// a changed poster URL means changed art, so updated data is saved and
    /// later reopens (and restarts) paint the grid + detail header from the
    /// entry without waiting for a meta fetch.
    pub(super) fn persist_poster_for(&self, id: &str, url: &str) {
        if url.is_empty() {
            return;
        }
        let changed = {
            let mut state = self.shared.lock().unwrap();
            match state.entries.iter_mut().find(|e| e.id == id) {
                Some(e) if e.poster_url != url => {
                    e.poster_url = url.to_string();
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.persist_library();
        }
    }

    /// Record header text on a saved library entry (no-op when unchanged or
    /// not in library). Backfills entries saved before genres/description
    /// were persisted, so later reopens paint pills + synopsis without
    /// waiting for a meta fetch. By default only empty slots are filled
    /// (avoids addon churn); with `overwrite` (unfinished-show refresh)
    /// fresh non-empty text replaces already-stored text so updated data
    /// is saved.
    pub(super) fn persist_header_for(
        &self,
        id: &str,
        genres: &[String],
        description: &str,
        year: &str,
        overwrite: bool,
    ) {
        let changed = {
            let mut state = self.shared.lock().unwrap();
            match state.entries.iter_mut().find(|e| e.id == id) {
                Some(e) => merge_library_header_text(e, genres, description, year, overwrite),
                None => false,
            }
        };
        if changed {
            self.persist_library();
        }
    }

    /// Load the detail page backdrop off the UI thread (same guards as the
    /// poster path). Missing/failed backdrops simply keep the gradient
    /// placeholder.
    pub(super) fn load_detail_backdrop(&self, url: String, item_id: String) {
        let bridge = self.clone();
        net::fetch_image(url, Some(DETAIL_BACKDROP_SIDE), move |pixels| {
            let Some(pixels) = pixels else {
                return; // keep the placeholder
            };
            let _ = slint::invoke_from_event_loop(move || {
                let Some(app) = bridge.app() else {
                    return;
                };
                if !app.get_modal_visible() {
                    return;
                }
                let still_current = {
                    let state = bridge.shared.lock().unwrap();
                    state
                        .modal_item
                        .as_ref()
                        .map(|m| m.id == item_id)
                        .unwrap_or(false)
                };
                if still_current {
                    app.set_selected_backdrop(Image::from_rgba8(pixels));
                }
            });
        });
    }

    /// Load the detail page poster off the UI thread so opening an item is
    /// instant even when the cached image still needs decoding (or the poster
    /// must be downloaded). The update is dropped if the detail page closed
    /// or switched to another item meanwhile.
    pub(super) fn load_detail_poster(&self, url: String, item_id: String) {
        let bridge = self.clone();
        net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
            let Some(pixels) = pixels else {
                return; // keep the placeholder
            };
            let _ = slint::invoke_from_event_loop(move || {
                let Some(app) = bridge.app() else {
                    return;
                };
                if !app.get_modal_visible() {
                    return;
                }
                let still_current = {
                    let state = bridge.shared.lock().unwrap();
                    state
                        .modal_item
                        .as_ref()
                        .map(|m| m.id == item_id)
                        .unwrap_or(false)
                };
                if still_current {
                    app.set_selected_poster(Image::from_rgba8(pixels));
                }
            });
        });
    }

    /// Refresh the detail-page poster without flashing: fresh bytes download
    /// in the background and the image repaints only when the pixels
    /// actually changed (identical art keeps the currently shown poster).
    /// The update is dropped if the detail page closed or switched to
    /// another item meanwhile.
    pub(super) fn refresh_detail_poster(&self, url: String, item_id: String) {
        let bridge = self.clone();
        refresh_image_if_changed(url, Some(DISPLAY_POSTER_SIDE), move |fresh| {
            let Some(pixels) = fresh else {
                return; // unchanged or failed: keep showing the current art
            };
            let _ = slint::invoke_from_event_loop(move || {
                // Grids bake poster Images into their row models: push the
                // fresh art there too, or the library/catalog card would
                // keep stale pixels until a rebuild or restart. Runs even
                // when the modal already closed (keyed by id, not by view).
                bridge.push_poster_to_grids(&item_id, &pixels);
                let Some(app) = bridge.app() else {
                    return;
                };
                if !app.get_modal_visible() {
                    return;
                }
                let still_current = {
                    let state = bridge.shared.lock().unwrap();
                    state
                        .modal_item
                        .as_ref()
                        .map(|m| m.id == item_id)
                        .unwrap_or(false)
                };
                if still_current {
                    app.set_selected_poster(Image::from_rgba8(pixels));
                }
            });
        });
    }

    /// Push fresh poster pixels into any grid card showing `item_id`
    /// (Discover catalog + Library): row models bake `Image` values, so
    /// without this a refreshed poster would only appear after a grid
    /// rebuild or restart. Desktop also drops the matching `PosterStore`
    /// entries so later repaints can't resurrect the stale art. Main thread
    /// only (touches the Slint models).
    pub(super) fn push_poster_to_grids(
        &self,
        item_id: &str,
        pixels: &SharedPixelBuffer<Rgba8Pixel>,
    ) {
        let Some(app) = self.app() else {
            return;
        };
        let img = Image::from_rgba8(pixels.clone());
        for library in [false, true] {
            let model = if library {
                app.get_library()
            } else {
                app.get_catalog()
            };
            let len = model.row_count();
            for i in 0..len {
                let matches = model
                    .row_data(i)
                    .map(|card: MediaCard| card.id.as_str() == item_id)
                    .unwrap_or(false);
                if !matches {
                    continue;
                }
                if library {
                    app.invoke_set_library_poster(i as i32, img.clone());
                } else {
                    app.invoke_set_card_poster(i as i32, img.clone());
                }
                #[cfg(feature = "desktop")]
                {
                    let generation = self.catalog_gen.load(Ordering::Relaxed);
                    self.poster_cache.lock().unwrap().remove(&(generation, i));
                }
            }
        }
    }

    /// Refresh the detail-page backdrop without flashing (same contract as
    /// `refresh_detail_poster`).
    pub(super) fn refresh_detail_backdrop(&self, url: String, item_id: String) {
        let bridge = self.clone();
        refresh_image_if_changed(url, Some(DETAIL_BACKDROP_SIDE), move |fresh| {
            let Some(pixels) = fresh else {
                return; // unchanged or failed: keep showing the current art
            };
            let _ = slint::invoke_from_event_loop(move || {
                let Some(app) = bridge.app() else {
                    return;
                };
                if !app.get_modal_visible() {
                    return;
                }
                let still_current = {
                    let state = bridge.shared.lock().unwrap();
                    state
                        .modal_item
                        .as_ref()
                        .map(|m| m.id == item_id)
                        .unwrap_or(false)
                };
                if still_current {
                    app.set_selected_backdrop(Image::from_rgba8(pixels));
                }
            });
        });
    }
}

pub(crate) fn set_active_cache_settings(settings: CacheSettings) {
    // The decoded LRU budget lives in `nova-media`; the settings themselves are
    // shared through `nova-config` (read by the media/player crates).
    nova_media::cache::set_decoded_cache_budget(settings.lru_cache_mb);
    set_cache_settings(settings);
}

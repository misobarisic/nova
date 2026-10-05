//! Poster / backdrop / episode-thumbnail image pipeline.
use super::*;

/// Future episode thumbnails can be declared before their image exists.
/// Keep the library poster as a fallback instead of a permanent placeholder.
fn fetch_home_card_art(
    url: String,
    fallback: String,
    then: impl FnOnce(Option<SharedPixelBuffer<Rgba8Pixel>>) + Send + 'static,
) {
    let has_fallback = !fallback.is_empty() && fallback != url;
    net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
        if pixels.is_some() || !has_fallback {
            then(pixels);
        } else {
            net::fetch_image(fallback, Some(DISPLAY_POSTER_SIDE), then);
        }
    });
}

// Resolve by current metadata identity, never by a row index captured before
// search ranking or catalog replacement. Preview URLs survive scroll unloading.
fn poster_indices(previews: &mut [MetaPreview], type_: &str, id: &str, url: &str) -> Vec<usize> {
    previews
        .iter_mut()
        .enumerate()
        .filter_map(|(index, preview)| {
            if preview.type_ == type_ && preview.id == id {
                preview.poster = Some(url.into());
                Some(index)
            } else {
                None
            }
        })
        .collect()
}
fn paint_poster_rows(
    model: &slint::ModelRc<MediaCard>,
    indices: &[usize],
    id: &str,
    url: &str,
    image: &Image,
) {
    let Some(rows) = model.as_any().downcast_ref::<VecModel<MediaCard>>() else {
        return;
    };
    for &index in indices {
        if let Some(mut row) = rows.row_data(index)
            && row.id.as_str() == id
        {
            row.poster_path = url.into();
            row.poster = image.clone();
            row.is_loaded = true;
            rows.set_row_data(index, row);
        }
    }
}

fn requested_poster_indices(model: &slint::ModelRc<MediaCard>, id: &str, url: &str) -> Vec<usize> {
    (0..model.row_count())
        .filter(|&index| {
            model
                .row_data(index)
                .is_some_and(|card| card.id.as_str() == id && card.poster_path.as_str() == url)
        })
        .collect()
}

fn artwork_recovery_target(
    state: &mut Shared,
    id: &str,
    failed_url: &str,
) -> Option<(String, String)> {
    let entry = state.entries.iter().find(|entry| entry.id == id)?;
    if entry.poster_url != failed_url {
        return None;
    }
    let target = (entry.type_.clone(), entry.id.clone());
    let key = (target.0.clone(), target.1.clone(), failed_url.into());
    let now = std::time::Instant::now();
    if state
        .library_artwork_recovery
        .get(&key)
        .is_some_and(|last| now.duration_since(*last) < Duration::from_secs(60))
    {
        return None;
    }
    state.library_artwork_recovery.insert(key, now);
    Some(target)
}

impl Bridge {
    /// Prefetch can discover an image missing/broken in the catalog preview.
    /// Decode only for currently visible models needing art, independent of
    /// their row order; full metadata prefetch remains controlled by settings.
    pub(super) fn load_discover_poster(&self, type_: String, id: String, url: String) {
        let Some(app) = self.app() else { return };
        let needed = [
            app.get_catalog(),
            app.get_search_results(),
            app.get_library(),
        ]
        .iter()
        .any(|model| {
            (0..model.row_count()).any(|i| {
                model
                    .row_data(i)
                    .is_some_and(|row| row.id.as_str() == id && !row.is_loaded)
            })
        });
        let saved = self
            .shared
            .lock()
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.type_ == type_ && entry.id == id && entry.poster_url != url);
        if !needed && !saved {
            return;
        }
        let bridge = self.clone();
        let poster_url = url.clone();
        net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
            if let Some(pixels) = pixels {
                let _ = slint::invoke_from_event_loop(move || {
                    bridge.publish_discover_poster(&type_, &id, &poster_url, &pixels);
                });
            }
        });
    }

    /// Successful Detail/prefetch art must reach browse, search and saved rows,
    /// including their URL so a scroll unload/reload cannot restore the old one.
    pub(super) fn publish_discover_poster(
        &self,
        type_: &str,
        id: &str,
        url: &str,
        pixels: &SharedPixelBuffer<Rgba8Pixel>,
    ) {
        let Some(app) = self.app() else { return };
        merge_meta_header_for(
            type_,
            id,
            &MetaHeader {
                poster_url: url.into(),
                ..Default::default()
            },
        );
        let (browse, search, library) = {
            let mut state = self.shared.lock().unwrap();
            let browse = poster_indices(&mut state.previews, type_, id, url);
            let search = poster_indices(&mut state.search_previews, type_, id, url);
            let library = state
                .entries
                .iter()
                .any(|entry| entry.id == id && entry.type_ == type_);
            (browse, search, library)
        };
        let image = Image::from_rgba8(pixels.clone());
        paint_poster_rows(&app.get_catalog(), &browse, id, url, &image);
        paint_poster_rows(&app.get_search_results(), &search, id, url, &image);
        if library {
            let changed = self
                .shared
                .lock()
                .unwrap()
                .entries
                .iter()
                .any(|entry| entry.id == id && entry.type_ == type_ && entry.poster_url != url);
            self.persist_poster_for(id, url);
            let model = app.get_library();
            let indices = (0..model.row_count())
                .filter(|&i| model.row_data(i).is_some_and(|row| row.id.as_str() == id))
                .collect::<Vec<_>>();
            paint_poster_rows(&model, &indices, id, url, &image);
            if changed {
                self.apply_home_to_ui();
                self.dispatch_continue_posters();
                self.dispatch_upcoming_posters();
            }
        }
        #[cfg(feature = "desktop")]
        for index in browse {
            let generation = self.catalog_gen.load(Ordering::Relaxed);
            self.poster_cache
                .lock()
                .unwrap()
                .remove(&(generation, index));
        }
    }

    fn detail_poster_stale(&self, id: &str, url: &str) -> bool {
        let desired = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .filter(|item| item.id == id)
            .map(|item| item.poster_url.clone());
        desired.is_some_and(|desired| {
            desired != url
                && decoded_cache_get(&sized_cache_key(&desired, Some(DISPLAY_POSTER_SIDE)))
                    .is_some()
        })
    }

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
        let requested_url = url.clone();
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
                    && app.get_search_results().row_data(index).is_some_and(|row| {
                        row.id.as_str() == id && row.poster_path.as_str() == requested_url
                    })
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
            let requested_url = url.clone();
            net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
                let Some(pixels) = pixels else {
                    return;
                };
                let _ = slint::invoke_from_event_loop(move || {
                    if generation != gen_counter.load(Ordering::Relaxed) {
                        return;
                    }
                    if let Some(app) = app_weak.upgrade() {
                        let model = if library {
                            app.get_library()
                        } else {
                            app.get_catalog()
                        };
                        if model
                            .row_data(index)
                            .is_none_or(|row| row.poster_path.as_str() != requested_url)
                        {
                            return;
                        }
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
                    Bridge::schedule_episode_thumb_render(worker);
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
                Bridge::schedule_episode_thumb_render(worker);
            });
        }
    }

    /// Debounced episode-list re-render after thumbnail downloads: at most
    /// one render is scheduled; it picks up every thumbnail finished so far.
    /// Render the current list: an older item's pending timer must not swallow
    /// completions from a newly opened item/season. Rows read pixels by their
    /// current URLs, so an old download cannot apply another show's artwork.
    pub(super) fn schedule_episode_thumb_render(bridge: Bridge) {
        if !EPISODE_RENDER_PENDING.swap(true, Ordering::SeqCst) {
            let _ = slint::invoke_from_event_loop(move || {
                let timer = slint::Timer::default();
                timer.start(
                    slint::TimerMode::SingleShot,
                    Duration::from_millis(120),
                    move || {
                        EPISODE_RENDER_PENDING.store(false, Ordering::SeqCst);
                        if bridge
                            .app()
                            .is_some_and(|app| app.get_modal_visible() && app.get_modal_episodes())
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
                if let Some(url) = season_thumb_url(&m.videos, &m.season_backdrops, s)
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
        for entry in entries {
            if entry.poster_url.is_empty() {
                self.recover_library_artwork(&entry.id, "");
                continue;
            }
            let url = entry.poster_url.clone();
            let id = entry.id.clone();
            let bridge = self.clone();
            net::fetch_image(url.clone(), Some(DISPLAY_POSTER_SIDE), move |pixels| {
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(app) = bridge.app() else { return };
                    let model = app.get_library();
                    // Filtering/progress refreshes can reorder the model while
                    // decoding. Resolve the current row, and reject old URLs.
                    let indices = requested_poster_indices(&model, &id, &url);
                    if let Some(pixels) = pixels {
                        paint_poster_rows(&model, &indices, &id, &url, &Image::from_rgba8(pixels));
                    } else {
                        bridge.recover_library_artwork(&id, &url);
                    }
                });
            });
        }
    }

    /// Old saved URLs can expire even when episodes/header text are cached.
    /// Keep native IDs; ask the same metadata pipeline used by Detail for art.
    fn recover_library_artwork(&self, id: &str, failed_url: &str) {
        let target = {
            let mut state = self.shared.lock().unwrap();
            let Some(target) = artwork_recovery_target(&mut state, id, failed_url) else {
                return;
            };
            target
        };
        // Reuse a previously decoded replacement immediately; also refresh
        // metadata so a stale cached URL doesn't block recovery.
        if let Some(header) = read_meta_header_for(&target.0, &target.1)
            && !header.poster_url.is_empty()
            && header.poster_url != failed_url
        {
            self.load_discover_poster(target.0.clone(), target.1.clone(), header.poster_url);
        }
        self.prefetch_meta_pairs(vec![target]);
    }

    /// Fetch the Home card's selected thumbnail/poster off the UI thread.
    /// Guard both series and URL: the next episode can occupy the same slot.
    pub(super) fn dispatch_continue_posters(&self) {
        let Some(app) = self.app() else { return };
        let fallbacks: HashMap<_, _> = self
            .shared
            .lock()
            .unwrap()
            .entries
            .iter()
            .map(|entry| (entry.id.clone(), entry.poster_url.clone()))
            .collect();
        let items: Vec<_> = app
            .get_home_continue()
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                decoded_cache_get(&sized_cache_key(&row.art_url, Some(DISPLAY_POSTER_SIDE)))
                    .is_none()
            })
            .map(|(index, row)| {
                let fallback = fallbacks.get(row.id.as_str()).cloned().unwrap_or_default();
                (index, row.id, row.art_url, fallback)
            })
            .collect();
        let app_weak = self.app.clone();
        for (_index, id, url, fallback) in items {
            let weak = app_weak.clone();
            let bridge = self.clone();
            let failed_poster = fallback.clone();
            fetch_home_card_art(url.to_string(), fallback, move |pixels| {
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(pixels) = pixels else {
                        bridge.recover_library_artwork(id.as_str(), &failed_poster);
                        return;
                    };
                    let Some(app) = weak.upgrade() else { return };
                    let model = app.get_home_continue();
                    if let Some((index, mut row)) = model
                        .iter()
                        .enumerate()
                        .find(|(_, row)| row.id == id && row.art_url == url)
                    {
                        row.poster = Image::from_rgba8(
                            decoded_cache_get(&sized_cache_key(&url, Some(DISPLAY_POSTER_SIDE)))
                                .unwrap_or(pixels),
                        );
                        row.is_loaded = true;
                        model.set_row_data(index, row);
                    }
                });
            });
        }
    }

    /// Upcoming and its selected calendar day share the same guarded image.
    pub(super) fn dispatch_upcoming_posters(&self) {
        let Some(app) = self.app() else { return };
        let fallbacks: HashMap<_, _> = self
            .shared
            .lock()
            .unwrap()
            .entries
            .iter()
            .map(|entry| (entry.id.clone(), entry.poster_url.clone()))
            .collect();
        let items: Vec<_> = app
            .get_home_upcoming()
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                decoded_cache_get(&sized_cache_key(&row.art_url, Some(DISPLAY_POSTER_SIDE)))
                    .is_none()
            })
            .map(|(index, row)| {
                let fallback = fallbacks.get(row.id.as_str()).cloned().unwrap_or_default();
                (index, row.id, row.art_url, fallback)
            })
            .collect();
        let app_weak = self.app.clone();
        for (_index, id, url, fallback) in items {
            let weak = app_weak.clone();
            let bridge = self.clone();
            let failed_poster = fallback.clone();
            fetch_home_card_art(url.to_string(), fallback, move |pixels| {
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(pixels) = pixels else {
                        bridge.recover_library_artwork(id.as_str(), &failed_poster);
                        return;
                    };
                    let Some(app) = weak.upgrade() else { return };
                    let model = app.get_home_upcoming();
                    if let Some((index, mut row)) = model
                        .iter()
                        .enumerate()
                        .find(|(_, row)| row.id == id && row.art_url == url)
                    {
                        let image = Image::from_rgba8(
                            decoded_cache_get(&sized_cache_key(&url, Some(DISPLAY_POSTER_SIDE)))
                                .unwrap_or(pixels),
                        );
                        row.poster = image.clone();
                        row.is_loaded = true;
                        model.set_row_data(index, row);
                        // A day can contain several episodes of the same series;
                        // match its selected artwork rather than series alone.
                        let day_model = app.get_home_cal_day();
                        let matches: Vec<_> = day_model
                            .iter()
                            .enumerate()
                            .filter(|(_, row)| row.id == id && row.art_url == url)
                            .collect();
                        for (day_index, mut row) in matches {
                            row.poster = image.clone();
                            row.is_loaded = true;
                            day_model.set_row_data(day_index, row);
                        }
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
    /// must be downloaded). Successful art also updates matching grid rows;
    /// only the detail-view update is dropped after closing or switching items.
    pub(super) fn load_detail_poster(&self, url: String, item_id: String) {
        let media_type = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .filter(|item| item.id == item_id)
            .map(|item| item.type_.clone());
        let poster_url = url.clone();
        let bridge = self.clone();
        net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
            let Some(pixels) = pixels else {
                return; // keep the placeholder
            };
            let _ = slint::invoke_from_event_loop(move || {
                if bridge.detail_poster_stale(&item_id, &poster_url) {
                    return;
                }
                let Some(app) = bridge.app() else {
                    return;
                };
                if let Some(type_) = &media_type {
                    bridge.publish_discover_poster(type_, &item_id, &poster_url, &pixels);
                }
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
    /// Matching grid rows also receive changed art after Detail closes;
    /// the detail-view update still checks the currently selected item.
    pub(super) fn refresh_detail_poster(&self, url: String, item_id: String) {
        let media_type = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .filter(|item| item.id == item_id)
            .map(|item| item.type_.clone());
        let poster_url = url.clone();
        let bridge = self.clone();
        refresh_image_if_changed(url, Some(DISPLAY_POSTER_SIDE), move |fresh| {
            let Some(pixels) = fresh else {
                return; // unchanged or failed: keep showing the current art
            };
            let _ = slint::invoke_from_event_loop(move || {
                if bridge.detail_poster_stale(&item_id, &poster_url) {
                    return;
                }
                // Grids bake poster Images into their row models: push the
                // fresh art there too, or the library/catalog card would
                // keep stale pixels until a rebuild or restart. Runs even
                // when the modal already closed (keyed by id, not by view).
                if let Some(type_) = &media_type {
                    bridge.publish_discover_poster(type_, &item_id, &poster_url, &pixels);
                }
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

#[cfg(test)]
mod discover_artwork_tests {
    use super::*;
    #[test]
    fn old_saved_artwork_recovers_once_without_replacing_native_identity() {
        let mut state = Shared::default();
        state.entries.push(LibraryEntry {
            id: "native:old-show".into(),
            type_: "series".into(),
            poster_url: "https://old.example/expired.jpg".into(),
            name: String::new(),
            year: String::new(),
            background_url: String::new(),
            genres: vec![],
            description: String::new(),
            categories: vec![],
            watch_status: WatchStatus::Auto,
            added_at_secs: 0,
        });
        let old = state.entries[0].poster_url.clone();
        assert_eq!(
            artwork_recovery_target(&mut state, "native:old-show", &old),
            Some(("series".into(), "native:old-show".into()))
        );
        // Library and Home can fail together; share one metadata request.
        assert!(artwork_recovery_target(&mut state, "native:old-show", &old).is_none());
        // A temporary offline failure can retry on a later page refresh.
        state
            .library_artwork_recovery
            .values_mut()
            .for_each(|last| {
                *last -= Duration::from_secs(61);
            });
        assert!(artwork_recovery_target(&mut state, "native:old-show", &old).is_some());
        state.entries[0].poster_url = "https://new.example/poster.jpg".into();
        assert!(artwork_recovery_target(&mut state, "native:old-show", &old).is_none());
        assert!(artwork_recovery_target(&mut state, "removed", &old).is_none());
    }

    #[test]
    fn saved_artwork_completion_follows_reordered_rows_and_rejects_expired_url() {
        let rows = Rc::new(VecModel::from(vec![
            MediaCard {
                id: "other".into(),
                poster_path: "old".into(),
                ..Default::default()
            },
            MediaCard {
                id: "saved".into(),
                poster_path: "new".into(),
                ..Default::default()
            },
            MediaCard {
                id: "saved".into(),
                poster_path: "old".into(),
                ..Default::default()
            },
        ]));
        let model: slint::ModelRc<MediaCard> = rows.clone().into();
        let indices = requested_poster_indices(&model, "saved", "old");
        assert_eq!(indices, vec![2]);
        paint_poster_rows(
            &model,
            &indices,
            "saved",
            "old",
            &Image::from_rgba8(SharedPixelBuffer::new(1, 1)),
        );
        assert!(!rows.row_data(0).unwrap().is_loaded);
        assert!(!rows.row_data(1).unwrap().is_loaded);
        assert!(rows.row_data(2).unwrap().is_loaded);
    }

    #[test]
    fn published_posters_follow_type_and_current_identity_and_survive_unloading() {
        let mut previews = vec![
            MetaPreview {
                id: "show".into(),
                type_: "movie".into(),
                ..Default::default()
            },
            MetaPreview {
                id: "show".into(),
                type_: "series".into(),
                ..Default::default()
            },
            MetaPreview {
                id: "other".into(),
                type_: "series".into(),
                ..Default::default()
            },
        ];
        let rows = Rc::new(VecModel::from(vec![
            MediaCard {
                id: "show".into(),
                ..Default::default()
            },
            MediaCard {
                id: "show".into(),
                ..Default::default()
            },
            MediaCard {
                id: "other".into(),
                ..Default::default()
            },
        ]));
        let model: slint::ModelRc<MediaCard> = rows.clone().into();
        let image = Image::from_rgba8(SharedPixelBuffer::new(1, 1));
        let url = "https://images.example/detail.jpg";
        let indices = poster_indices(&mut previews, "series", "show", url);
        assert_eq!(indices, [1]);
        paint_poster_rows(&model, &indices, "show", url, &image);
        assert!(!rows.row_data(0).unwrap().is_loaded);
        assert!(!rows.row_data(2).unwrap().is_loaded);
        let mut painted = rows.row_data(1).unwrap();
        assert!(painted.is_loaded);
        assert_eq!(painted.poster.size().width, 1);
        assert_eq!(painted.poster_path.as_str(), url);
        painted.poster = Image::default();
        painted.is_loaded = false;
        rows.set_row_data(1, painted);
        assert_eq!(rows.row_data(1).unwrap().poster_path.as_str(), url);
        assert_eq!(previews[1].poster.as_deref(), Some(url));
        // A reordered model cannot let an old numeric index paint a new title.
        rows.set_row_data(
            1,
            MediaCard {
                id: "other".into(),
                ..Default::default()
            },
        );
        paint_poster_rows(&model, &indices, "show", url, &image);
        assert!(!rows.row_data(1).unwrap().is_loaded);
        previews.swap(1, 2);
        let indices = poster_indices(&mut previews, "series", "show", url);
        assert_eq!(indices, [2]);
    }
}

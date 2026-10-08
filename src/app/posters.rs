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

fn artwork_request_current(state: &Shared, type_: &str, id: &str, failed_url: &str) -> bool {
    let current = state
        .entries
        .iter()
        .any(|entry| entry.type_ == type_ && entry.id == id && entry.poster_url == failed_url)
        || state
            .previews
            .iter()
            .chain(&state.search_previews)
            .any(|preview| {
                preview.type_ == type_
                    && preview.id == id
                    && preview.poster.as_deref().unwrap_or_default() == failed_url
            })
        || state.home_catalog_row_items.iter().any(|preview| {
            preview.type_ == type_
                && preview.id == id
                && preview.poster.as_deref().unwrap_or_default() == failed_url
        })
        || state.modal_item.as_ref().is_some_and(|item| {
            item.type_ == type_ && item.id == id && item.poster_url == failed_url
        });
    current && !type_.is_empty() && !id.is_empty()
}

fn grid_artwork_recovery_target(
    state: &mut Shared,
    type_: &str,
    id: &str,
    failed_url: &str,
) -> Option<(String, String)> {
    if !artwork_request_current(state, type_, id, failed_url) {
        return None;
    }
    let target = (type_.to_owned(), id.to_owned());
    let key = (target.0.clone(), target.1.clone(), failed_url.into());
    let now = std::time::Instant::now();
    if state
        .artwork_recovery
        .get(&key)
        .is_some_and(|last| now.duration_since(*last) < Duration::from_secs(60))
    {
        return None;
    }
    state.artwork_recovery.insert(key, now);
    Some(target)
}

/// Only the broker's current, conflict-free connection can cross native IDs.
fn mapped_poster_endpoints(
    item: &MetaItem,
    type_: &str,
    installed: &[Installed],
) -> Vec<(String, String)> {
    let extra = |key: &str| item.extra.get(key).or_else(|| item.preview.extra.get(key));
    if extra("novaMetadataRevision").and_then(serde_json::Value::as_u64)
        != Some(nova_providers::metadata_revision())
    {
        return vec![];
    }
    extra("novaConnections")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .take(6)
        .filter_map(|value| {
            let connection: nova_providers::MetadataConnection =
                serde_json::from_value(value.clone()).ok()?;
            if connection.basis == "conflicting-identifiers" || connection.media_id.is_empty() {
                return None;
            }
            let addon = installed.iter().find(|addon| {
                addon.enabled
                    && addon.available
                    && nova_providers::addon_metadata_id(&addon.url) == connection.addon_id
                    && addon.manifest.accepts("meta", type_, &connection.media_id)
            })?;
            let endpoint = Addon::new(&addon.url)
                .ok()?
                .meta_url(type_, &connection.media_id);
            Some((endpoint, connection.media_id))
        })
        .collect()
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
        }) || app
            .get_home_catalog_cards()
            .iter()
            .any(|card| card.media_type == type_ && card.id == id && card.poster_url == url);
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
        let update_detail = {
            let mut state = self.shared.lock().unwrap();
            state.modal_item.as_mut().is_some_and(|item| {
                if item.id != id
                    || item.type_ != type_
                    || (item.poster_url != url
                        && decoded_cache_contains(&sized_cache_key(
                            &item.poster_url,
                            Some(DISPLAY_POSTER_SIDE),
                        )))
                {
                    return false;
                }
                item.poster_url = url.into();
                true
            })
        };
        if update_detail && app.get_modal_visible() {
            app.set_selected_poster(image.clone());
        }
        paint_poster_rows(&app.get_catalog(), &browse, id, url, &image);
        paint_poster_rows(&app.get_search_results(), &search, id, url, &image);
        let home_cards = app.get_home_catalog_cards();
        for (index, mut card) in home_cards.iter().enumerate() {
            if card.id == id && card.media_type == type_ {
                card.poster_url = url.into();
                card.poster = image.clone();
                card.is_loaded = true;
                home_cards.set_row_data(index, card);
            }
        }
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
        if url.is_empty() {
            self.recover_missing_artwork(&type_, &id, "");
            return;
        }
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
                if let (Some(index), Some(app)) = (index, app_weak.upgrade())
                    && app.get_search_results().row_data(index).is_some_and(|row| {
                        row.id.as_str() == id && row.poster_path.as_str() == requested_url
                    })
                {
                    if let Some(pixels) = pixels {
                        app.invoke_set_search_card_poster(index as i32, Image::from_rgba8(pixels));
                    } else {
                        bridge.recover_missing_artwork(&type_, &id, &requested_url);
                    }
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
            let bridge = self.clone();
            let requested_url = url.clone();
            net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(pixels) = pixels else {
                        if !library {
                            bridge.recover_catalog_artwork(generation, index, &requested_url);
                        }
                        return;
                    };
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
        let Some(app) = self.app() else { return };
        let model = app.get_library();
        for entry in entries {
            if model.iter().any(|row| {
                row.id.as_str() == entry.id
                    && row.poster_path.as_str() == entry.poster_url
                    && row.is_loaded
            }) {
                continue;
            }
            if entry.poster_url.is_empty() {
                self.recover_library_artwork(&entry.id, "");
                continue;
            }
            let url = entry.poster_url.clone();
            let id = entry.id.clone();
            let key = (entry.type_.clone(), id.clone(), url.clone());
            if !self
                .shared
                .lock()
                .unwrap()
                .library_poster_inflight
                .insert(key.clone())
            {
                continue;
            }
            let bridge = self.clone();
            net::fetch_image(url.clone(), Some(DISPLAY_POSTER_SIDE), move |pixels| {
                let _ = slint::invoke_from_event_loop(move || {
                    let mut state = bridge.shared.lock().unwrap();
                    state.library_poster_inflight.remove(&key);
                    let current = state.entries.iter().any(|entry| {
                        entry.type_ == key.0 && entry.id == id && entry.poster_url == url
                    });
                    drop(state);
                    if !current {
                        return;
                    }
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
        let type_ = self
            .shared
            .lock()
            .unwrap()
            .entries
            .iter()
            .find(|entry| entry.id == id && entry.poster_url == failed_url)
            .map(|entry| entry.type_.clone());
        if let Some(type_) = type_ {
            self.recover_missing_artwork(&type_, id, failed_url);
        }
    }

    pub(super) fn recover_missing_artwork(&self, type_: &str, id: &str, failed_url: &str) {
        let target = {
            let mut state = self.shared.lock().unwrap();
            if !artwork_request_current(&state, type_, id, failed_url) {
                return;
            }
            // A pending manifest is not a failed metadata request. Don't spend
            // the cooldown before any source can service this native identity.
            if !state.installed.iter().any(|addon| {
                addon.enabled && addon.available && addon.manifest.accepts("meta", type_, id)
            }) {
                if state.installed.is_empty()
                    || state
                        .installed
                        .iter()
                        .any(|addon| addon.enabled && !addon.available)
                {
                    state.pending_artwork_recovery.insert((
                        type_.into(),
                        id.into(),
                        failed_url.into(),
                    ));
                }
                return;
            }
            state
                .pending_artwork_recovery
                .remove(&(type_.into(), id.into(), failed_url.into()));
            grid_artwork_recovery_target(&mut state, type_, id, failed_url)
        };
        if let Some(target) = target {
            self.recover_artwork_target(target, failed_url);
        }
    }

    /// Only retry observed failures/missing URLs. An unloaded row may simply
    /// be decoding or outside the viewport; neither needs metadata recovery.
    pub(super) fn retry_pending_artwork(&self) {
        let targets = std::mem::take(&mut self.shared.lock().unwrap().pending_artwork_recovery);
        for (type_, id, url) in targets {
            self.recover_missing_artwork(&type_, &id, &url);
        }
    }

    /// A catalog worker's numeric slot is only valid for its original request.
    pub(super) fn recover_catalog_artwork(&self, generation: u64, index: usize, failed_url: &str) {
        if self.catalog_gen.load(Ordering::Relaxed) != generation {
            return;
        }
        let target = self
            .shared
            .lock()
            .unwrap()
            .previews
            .get(index)
            .filter(|preview| preview.poster.as_deref().unwrap_or_default() == failed_url)
            .map(|preview| (preview.type_.clone(), preview.id.clone()));
        if let Some((type_, id)) = target {
            self.recover_missing_artwork(&type_, &id, failed_url);
        }
    }

    /// Artwork is essential grid content, independent of episode prefetch.
    /// Do not stop at the first metadata response: its image may also be bad.
    fn recover_artwork_target(&self, (type_, id): (String, String), failed_url: &str) {
        let urls = self
            .shared
            .lock()
            .unwrap()
            .installed
            .iter()
            .filter(|addon| {
                addon.enabled && addon.available && addon.manifest.accepts("meta", &type_, &id)
            })
            .filter_map(|addon| Addon::new(&addon.url).ok())
            .map(|addon| (addon.meta_url(&type_, &id), id.clone()))
            .collect();
        fn next(
            bridge: Bridge,
            type_: String,
            id: String,
            mut urls: VecDeque<(String, String)>,
            mut tried: HashSet<String>,
        ) {
            let job = loop {
                let Some((url, expected_id)) = urls.pop_front() else {
                    return;
                };
                if tried.len() >= 16 {
                    return;
                }
                if tried.insert(url.clone()) {
                    break (url, expected_id);
                }
            };
            net::fetch_bytes(job.0, move |result| {
                let item = result
                    .ok()
                    .and_then(|bytes| Addon::parse_meta(&bytes).ok().flatten())
                    .filter(|item| meta_matches_request(item, &type_, &job.1));
                if let Some(item) = &item {
                    let saved = bridge
                        .shared
                        .lock()
                        .unwrap()
                        .entries
                        .iter()
                        .any(|entry| entry.type_ == type_ && entry.id == id);
                    if saved && bridge.cache_background_meta(&type_, &id, item) > 0 {
                        let bridge = bridge.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            bridge.refresh_library_progress_ui()
                        });
                    }
                    let mapped = mapped_poster_endpoints(
                        item,
                        &type_,
                        &bridge.shared.lock().unwrap().installed,
                    );
                    urls.extend(mapped.into_iter().filter(|(url, _)| !tried.contains(url)));
                }
                let poster = item
                    .and_then(|item| item.preview.poster)
                    .filter(|poster| !poster.trim().is_empty());
                let Some(poster) = poster else {
                    next(bridge, type_, id, urls, tried);
                    return;
                };
                net::fetch_image(poster.clone(), Some(DISPLAY_POSTER_SIDE), move |pixels| {
                    if let Some(pixels) = pixels {
                        let _ = slint::invoke_from_event_loop(move || {
                            bridge.publish_discover_poster(&type_, &id, &poster, &pixels);
                        });
                    } else {
                        next(bridge, type_, id, urls, tried);
                    }
                });
            });
        }
        let cached = read_meta_header_for(&type_, &id)
            .map(|header| header.poster_url)
            .filter(|url| !url.is_empty() && url != failed_url);
        if let Some(url) = cached {
            // A previously decoded alternative can fix the card without any
            // metadata request. Never retry the URL that just failed here.
            let bridge = self.clone();
            net::fetch_image(url.clone(), Some(DISPLAY_POSTER_SIDE), move |pixels| {
                if let Some(pixels) = pixels {
                    let _ = slint::invoke_from_event_loop(move || {
                        bridge.publish_discover_poster(&type_, &id, &url, &pixels);
                    });
                } else {
                    next(bridge, type_, id, urls, HashSet::new());
                }
            });
        } else {
            next(self.clone(), type_, id, urls, HashSet::new());
        }
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

    /// Fetch and paint the currently selected Home catalog rows. Each
    /// completion checks its current flattened index, identity and URL so a
    /// settings change or a newer catalog response cannot paint stale art.
    pub(super) fn dispatch_home_catalog_posters(&self) {
        let Some(app) = self.app() else { return };
        let requests = app
            .get_home_catalog_cards()
            .iter()
            .enumerate()
            .filter(|(_, card)| !card.is_loaded)
            .map(|(index, card)| {
                (
                    index,
                    card.media_type.to_string(),
                    card.id.to_string(),
                    card.poster_url.to_string(),
                )
            })
            .collect::<Vec<_>>();
        for (index, type_, id, url) in requests {
            if url.is_empty() {
                self.recover_missing_artwork(&type_, &id, "");
                continue;
            }
            let bridge = self.clone();
            let requested_url = url.clone();
            net::fetch_image(url.clone(), Some(DISPLAY_POSTER_SIDE), move |pixels| {
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(app) = bridge.app() else { return };
                    let model = app.get_home_catalog_cards();
                    let current = model.row_data(index).is_some_and(|card| {
                        card.id == id
                            && card.media_type == type_
                            && card.poster_url == requested_url
                    });
                    if let Some(pixels) = pixels {
                        if current {
                            bridge.publish_discover_poster(&type_, &id, &requested_url, &pixels);
                        }
                    } else if current {
                        bridge.recover_missing_artwork(&type_, &id, &requested_url);
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

    /// Full-fidelity decoding avoids the opaque JPEG poster derivative.
    /// Guard both opening identity and URL: late logos cannot repaint a
    /// different show or overwrite a newer metadata response.
    pub(super) fn load_current_detail_logo(&self) {
        let Some(app) = self.app() else { return };
        let request = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .map(|item| (item.open_token.clone(), item.logo_url.clone()));
        let Some((token, url)) = request else { return };
        let cached = transparent_title_logo(decoded_cache_get(&url));
        app.set_selected_logo(cached.map(Image::from_rgba8).unwrap_or_default());
        if url.is_empty() || app.get_selected_logo().size().width > 0 {
            return;
        }
        let bridge = self.clone();
        net::fetch_image(url.clone(), None, move |pixels| {
            let pixels = transparent_title_logo(pixels);
            let _ = slint::invoke_from_event_loop(move || {
                let Some(app) = bridge.app() else { return };
                let valid = app.get_modal_visible()
                    && bridge
                        .shared
                        .lock()
                        .unwrap()
                        .modal_item
                        .as_ref()
                        .is_some_and(|item| {
                            Arc::ptr_eq(&item.open_token, &token) && item.logo_url == url
                        });
                if valid {
                    app.set_selected_logo(pixels.map(Image::from_rgba8).unwrap_or_default());
                }
            });
        });
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
            let _ = slint::invoke_from_event_loop(move || {
                let Some(pixels) = pixels else {
                    if let Some(type_) = &media_type {
                        bridge.recover_missing_artwork(type_, &item_id, &poster_url);
                    }
                    return;
                };
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
                // None also means unchanged. Only a missing decoded image
                // needs recovery, such as an expired URL after clearing cache.
                if !decoded_cache_contains(&sized_cache_key(&poster_url, Some(DISPLAY_POSTER_SIDE)))
                {
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(type_) = &media_type {
                            bridge.recover_missing_artwork(type_, &item_id, &poster_url);
                        }
                    });
                }
                return;
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
    #[cfg(feature = "desktop")]
    #[test]
    fn artwork_recovery_and_detail_refresh() {
        // Isolate process-wide Slint, storage and image caches from other tests.
        const ROOT: &str = "NOVA_GRID_ARTWORK_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-grid-artwork-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            for mode in ["0", "1", "2", "3", "4", "5"] {
                let child_root = root.join(mode);
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .env(ROOT, &child_root)
                    .env("XDG_CACHE_HOME", child_root.join("cache"))
                    .env("NOVA_ARTWORK_MAP_TEST", mode)
                    .args([
                        "--exact",
                        "app::posters::discover_artwork_tests::artwork_recovery_and_detail_refresh",
                        "--nocapture",
                    ])
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "mode={mode} {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            fs::remove_dir_all(root).unwrap();
            return;
        };
        use std::io::{Read, Write};
        let root = PathBuf::from(root);
        let mode = std::env::var("NOVA_ARTWORK_MAP_TEST").unwrap();
        let detail = mode == "3";
        let cached_alternative = mode == "4";
        let pending_failure = mode == "5";
        let library = mode == "2" || detail || cached_alternative || pending_failure;
        let media_type = if detail { "movie" } else { "series" };
        let mapped = mode != "0";
        storage::init_at(&root);
        i_slint_backend_testing::init_integration_test_with_system_time();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let good = format!("{base}/good.png");
        let old = format!("{base}/old.png");
        let mut png = std::io::Cursor::new(vec![]);
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png = png.into_inner();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = done.clone();
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = requests.clone();
        let server_base = base.clone();
        let server = thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let (mut socket, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = [0; 4096];
                let size = socket.read(&mut bytes).unwrap();
                let path = String::from_utf8_lossy(&bytes[..size])
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_string();
                seen.lock().unwrap().push(path.clone());
                let (status, content, body) = if path.ends_with(".json") {
                    let art = if path.starts_with("/first/") {
                        "bad.png"
                    } else {
                        "good.png"
                    };
                    let id = if mapped && path.starts_with("/second/") {
                        "tt5095466"
                    } else {
                        "saved"
                    };
                    let mut meta = serde_json::json!({"id":id, "type":media_type, "name":"Saved", "poster":format!("{server_base}/{art}"),
                        "videos":[{"id":format!("{id}:1:1"), "season":1, "episode":1, "name":"Cached episode"}]});
                    if library {
                        // No invented broker connection: the real transport
                        // must confirm and apply the native-to-IMDb mapping.
                        meta["year"] = serde_json::json!("2020");
                        meta["videos"] = serde_json::json!([
                            {"id":format!("{id}:1:1"), "season":1, "episode":1, "name":"Opening", "released":"2020-01-01", "thumbnail":format!("{server_base}/good.png")},
                            {"id":format!("{id}:1:2"), "season":1, "episode":2, "name":"Arrival", "released":"2020-01-08", "thumbnail":format!("{server_base}/good.png")}
                        ]);
                        if detail {
                            meta["videos"] = serde_json::json!([]);
                        }
                        if path.starts_with("/first/") {
                            meta["poster"] = serde_json::Value::Null;
                            meta["imdb_id"] = serde_json::json!("tt5095466");
                        }
                    } else if mapped && path.starts_with("/first/") {
                        meta["novaMetadataRevision"] =
                            serde_json::json!(nova_providers::metadata_revision());
                        meta["novaConnections"] = serde_json::json!([{
                            "addonId":nova_providers::addon_metadata_id(&format!("{server_base}/second")),
                            "mediaId":"tt5095466", "ids":[], "basis":"exact-title-year"
                        }]);
                    }
                    (
                        "200 OK",
                        "application/json",
                        serde_json::to_vec(&serde_json::json!({"meta":meta})).unwrap(),
                    )
                } else if path == "/good.png" {
                    ("200 OK", "image/png", png.clone())
                } else {
                    ("404 Not Found", "text/plain", vec![])
                };
                write!(socket, "HTTP/1.1 {status}\r\nContent-Type: {content}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                socket.write_all(&body).unwrap();
            }
        });
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let (hi, _) = mpsc::channel();
        let (lo, _) = mpsc::channel();
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
            Arc::new(Mutex::new(PosterStore::new(1))),
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        let preview = MetaPreview {
            id: "saved".into(),
            type_: media_type.into(),
            name: "Saved".into(),
            ..Default::default()
        };
        let card = MediaCard {
            id: "saved".into(),
            ..Default::default()
        };
        app.set_library(Rc::new(VecModel::from(vec![card.clone()])).into());
        app.set_search_results(Rc::new(VecModel::from(vec![card])).into());
        {
            let mut state = bridge.shared.lock().unwrap();
            state.cache_settings.prefetch_metadata = false;
            state.search_previews = vec![preview.clone()];
            state.entries.push(LibraryEntry {
                id: "saved".into(),
                type_: media_type.into(),
                name: "Saved".into(),
                year: String::new(),
                poster_url: if library { old.clone() } else { String::new() },
                background_url: String::new(),
                genres: vec![],
                description: "Cached text".into(),
                categories: vec![],
                watch_status: WatchStatus::Auto,
                added_at_secs: 0,
            });
            state.installed = ["first", "second"].map(|path| Installed { url:format!("{base}/{path}"), label:path.into(), enabled:true,
                configure_ok:None, available:true, generation:0,
                manifest:serde_json::from_value(serde_json::json!({"id":path, "name":path, "version":"1", "types":[media_type],
                    "resources":[{"name":"meta", "types":[media_type], "idPrefixes": if mapped && path == "second" { vec!["tt"] } else { vec!["saved"] }}], "catalogs":[]})).unwrap()
            }).to_vec();
        }
        write_episodes_cache_for(
            media_type,
            "saved",
            &[Video {
                id: "saved:1:1".into(),
                season: Some(1),
                episode: Some(1),
                ..Default::default()
            }],
        );
        assert!(read_episodes_cache_for(media_type, "saved").is_some());
        let installed = std::mem::take(&mut bridge.shared.lock().unwrap().installed);
        bridge.recover_missing_artwork(media_type, "saved", "");
        assert!(
            bridge.shared.lock().unwrap().artwork_recovery.is_empty(),
            "pending manifests must not spend cooldowns"
        );
        bridge.shared.lock().unwrap().installed = installed;
        if library {
            let pixel = SharedPixelBuffer::<Rgba8Pixel>::new(1, 1);
            decoded_cache_insert(
                &sized_cache_key(&old, Some(DISPLAY_POSTER_SIDE)),
                pixel.clone(),
            );
            app.set_library(
                Rc::new(VecModel::from(vec![MediaCard {
                    id: "saved".into(),
                    poster_path: old.clone().into(),
                    poster: Image::from_rgba8(pixel),
                    is_loaded: true,
                    ..Default::default()
                }]))
                .into(),
            );
            write_meta_header_for(
                media_type,
                "saved",
                &MetaHeader {
                    poster_url: old.clone(),
                    description: "Cached description".into(),
                    ..Default::default()
                },
            );
            // Unloaded rows with a valid URL may just be waiting for decode.
            // Becoming available must not start speculative metadata repair.
            let mut preview = preview;
            preview.poster = Some(old.clone());
            let unloaded = Rc::new(VecModel::from(vec![MediaCard {
                id: "saved".into(),
                poster_path: old.clone().into(),
                ..Default::default()
            }]));
            app.set_search_results(unloaded.clone().into());
            app.set_catalog(unloaded.into());
            {
                let mut state = bridge.shared.lock().unwrap();
                state.search_previews = vec![preview.clone()];
                state.previews = vec![preview];
            }
            bridge.retry_pending_artwork();
            bridge.retry_pending_artwork();
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .pending_artwork_recovery
                    .is_empty()
            );
            assert!(bridge.shared.lock().unwrap().artwork_recovery.is_empty());
            net::init_metadata_transport();
            nova_providers::configure_metadata_addons(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .installed
                    .iter()
                    .map(|addon| nova_providers::MetadataAddon {
                        url: addon.url.clone(),
                        manifest: addon.manifest.clone(),
                    })
                    .collect(),
            );
            let pending = std::mem::take(&mut bridge.shared.lock().unwrap().installed);
            bridge.prefetch_library_meta();
            assert!(bridge.shared.lock().unwrap().metadata_prefetch.is_empty());
            bridge.shared.lock().unwrap().installed = pending;
            bridge.show_library_page();
            // Complete cached entries must not trigger metadata/mapping work.
            bridge.prefetch_library_meta();
            assert!(bridge.shared.lock().unwrap().metadata_prefetch.is_empty());
            assert!(bridge.shared.lock().unwrap().artwork_recovery.is_empty());
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .library_poster_inflight
                    .is_empty()
            );
            assert!(app.get_library().row_data(0).unwrap().is_loaded);
            if detail {
                // A healthy movie still rechecks metadata when explicitly
                // opened, while Library itself stays on its cached content.
                bridge.open_library_item(0);
            } else {
                if cached_alternative {
                    write_meta_header_for(
                        media_type,
                        "saved",
                        &MetaHeader {
                            poster_url: good.clone(),
                            description: "Cached description".into(),
                            ..Default::default()
                        },
                    );
                }
                if pending_failure {
                    for addon in &mut bridge.shared.lock().unwrap().installed {
                        addon.available = false;
                    }
                }
                // The saved URL stays unchanged. Clearing the image cache
                // exposes that it has expired, which must trigger mapping.
                decoded_cache_clear();
                app.set_library(
                    Rc::new(VecModel::from(vec![MediaCard {
                        id: "saved".into(),
                        poster_path: old.clone().into(),
                        ..Default::default()
                    }]))
                    .into(),
                );
                bridge.show_library_page();
                bridge.show_library_page();
                assert_eq!(
                    bridge.shared.lock().unwrap().library_poster_inflight.len(),
                    1
                );
            }
        } else {
            bridge.apply_catalog(0, vec![preview], false);
        }
        let weak = app.as_weak();
        let started = std::time::Instant::now();
        let expected_poster = good.clone();
        let retry_bridge = bridge.clone();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(20),
            move || {
                if pending_failure {
                    let retry = {
                        let mut state = retry_bridge.shared.lock().unwrap();
                        if state.pending_artwork_recovery.is_empty() {
                            false
                        } else {
                            assert!(state.artwork_recovery.is_empty());
                            for addon in &mut state.installed {
                                addon.available = true;
                            }
                            true
                        }
                    };
                    if retry {
                        retry_bridge.retry_pending_artwork();
                    }
                }
                if weak
                    .upgrade()
                    .unwrap()
                    .get_catalog()
                    .row_data(0)
                    .unwrap()
                    .poster_path
                    .as_str()
                    == expected_poster
                    || started.elapsed() > Duration::from_secs(10)
                {
                    slint::quit_event_loop().unwrap();
                }
            },
        );
        app.run().unwrap();
        done.store(true, Ordering::Relaxed);
        server.join().unwrap();
        assert_eq!(app.get_modal_visible(), detail);
        if detail {
            assert_eq!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .modal_item
                    .as_ref()
                    .unwrap()
                    .poster_url,
                good
            );
            assert!(app.get_selected_poster().size().width > 0);
        }
        for model in [
            app.get_catalog(),
            app.get_search_results(),
            app.get_library(),
        ] {
            let row = model.row_data(0).unwrap();
            assert!(row.is_loaded);
            assert_eq!(row.id.as_str(), "saved");
            assert_eq!(row.poster_path.as_str(), good);
        }
        assert_eq!(bridge.shared.lock().unwrap().entries[0].poster_url, good);
        assert_eq!(
            read_meta_header_for(media_type, "saved")
                .unwrap()
                .poster_url,
            good
        );
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .pending_artwork_recovery
                .is_empty()
                || !library
        );
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .library_poster_inflight
                .is_empty()
        );
        if library && !detail {
            assert_eq!(
                requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|path| path.as_str() == "/old.png")
                    .count(),
                1
            );
        }
        if library && !detail && !cached_alternative {
            let videos = read_episodes_cache_for(media_type, "saved").unwrap();
            assert_eq!(videos.len(), 2);
            assert_eq!(videos[0].id, "saved:1:1");
            assert_eq!(
                videos[0].extra["novaStreamIds"],
                serde_json::json!(["tt5095466:1:1"])
            );
            assert_eq!(
                videos[0].extra["novaMetadataRevision"].as_u64(),
                Some(nova_providers::metadata_revision())
            );
            assert_eq!(bridge.shared.lock().unwrap().artwork_recovery.len(), 1);
            assert_eq!(
                requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|path| path.starts_with("/first/meta/"))
                    .count(),
                1,
                "revisiting Library must not duplicate in-flight metadata work"
            );
        }
        if !detail {
            let expired = if library { "/old.png" } else { "/bad.png" };
            assert!(requests.lock().unwrap().iter().any(|path| path == expired));
        }
        if cached_alternative {
            assert!(
                requests
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|path| !path.contains("/meta/")),
                "a working cached alternative must avoid metadata requests"
            );
        } else {
            assert!(
                requests
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|path| path.starts_with("/second/meta/"))
            );
        }
    }

    #[test]
    fn alternate_poster_source_requires_current_nonconflicting_owned_mapping() {
        let addon = Installed { url:"https://metadata.example".into(), label:"Metadata".into(), enabled:true,
            configure_ok:None, available:true, generation:0,
            manifest:serde_json::from_value(serde_json::json!({"id":"meta", "name":"Meta", "version":"1", "types":["series"],
                "resources":[{"name":"meta", "types":["series"], "idPrefixes":["tt"]}], "catalogs":[]})).unwrap() };
        let revision = nova_providers::metadata_revision();
        let mut item = MetaItem::default();
        item.extra
            .insert("novaMetadataRevision".into(), serde_json::json!(revision));
        item.extra.insert("novaConnections".into(), serde_json::json!([{
            "addonId":nova_providers::addon_metadata_id(&addon.url), "mediaId":"tt5095466", "ids":[], "basis":"exact-title-year"
        }]));
        assert_eq!(
            mapped_poster_endpoints(&item, "series", std::slice::from_ref(&addon)),
            vec![(
                "https://metadata.example/meta/series/tt5095466.json".into(),
                "tt5095466".into()
            )]
        );
        item.extra.get_mut("novaConnections").unwrap()[0]["basis"] =
            serde_json::json!("conflicting-identifiers");
        assert!(mapped_poster_endpoints(&item, "series", std::slice::from_ref(&addon)).is_empty());
        item.extra.get_mut("novaConnections").unwrap()[0]["basis"] =
            serde_json::json!("exact-title-year");
        item.extra.insert(
            "novaMetadataRevision".into(),
            serde_json::json!(revision ^ 1),
        );
        assert!(mapped_poster_endpoints(&item, "series", std::slice::from_ref(&addon)).is_empty());
        item.extra
            .insert("novaMetadataRevision".into(), serde_json::json!(revision));
        let mut disabled = addon.clone();
        disabled.enabled = false;
        assert!(mapped_poster_endpoints(&item, "series", &[disabled]).is_empty());
        item.extra.get_mut("novaConnections").unwrap()[0]["addonId"] =
            serde_json::json!("unrelated-owner");
        assert!(mapped_poster_endpoints(&item, "series", &[addon]).is_empty());
    }

    #[test]
    fn detail_only_poster_failure_respects_current_typed_identity_and_url() {
        let mut state = Shared {
            modal_item: Some(ModalItem {
                open_token: Arc::new(()),
                pending_watch_now: None,
                episodes_loading: false,
                id: "detail-only".into(),
                type_: "movie".into(),
                request_id: "detail-only".into(),
                videos: vec![],
                season_backdrops: HashMap::new(),
                seasons: vec![],
                season_index: 0,
                episode_page: 0,
                name: "Detail".into(),
                year: String::new(),
                poster_url: "https://images.example/expired.jpg".into(),
                background_url: String::new(),
                logo_url: String::new(),
                description: String::new(),
                genres: vec![],
            }),
            ..Default::default()
        };
        assert!(
            grid_artwork_recovery_target(
                &mut state,
                "series",
                "detail-only",
                "https://images.example/expired.jpg"
            )
            .is_none()
        );
        assert!(
            grid_artwork_recovery_target(
                &mut state,
                "movie",
                "detail-only",
                "https://images.example/older.jpg"
            )
            .is_none()
        );
        assert!(
            grid_artwork_recovery_target(
                &mut state,
                "movie",
                "detail-only",
                "https://images.example/expired.jpg"
            )
            .is_some()
        );
        state.modal_item.as_mut().unwrap().poster_url = "https://images.example/current.jpg".into();
        assert!(
            grid_artwork_recovery_target(
                &mut state,
                "movie",
                "detail-only",
                "https://images.example/expired.jpg"
            )
            .is_none()
        );
    }

    #[test]
    fn grids_recover_artwork_without_library_membership_or_episode_prefetch() {
        let mut state = Shared::default();
        state.cache_settings.prefetch_metadata = false;
        state.previews.push(MetaPreview {
            id: "provider:show".into(),
            type_: "series".into(),
            poster: None,
            ..Default::default()
        });
        assert_eq!(
            grid_artwork_recovery_target(&mut state, "series", "provider:show", ""),
            Some(("series".into(), "provider:show".into()))
        );
        // Search and browse share the request cooldown.
        state.search_previews = state.previews.clone();
        assert!(grid_artwork_recovery_target(&mut state, "series", "provider:show", "").is_none());
        assert!(grid_artwork_recovery_target(&mut state, "movie", "provider:show", "").is_none());
        state.search_previews[0].poster = Some("broken".into());
        assert!(
            grid_artwork_recovery_target(&mut state, "series", "provider:show", "broken").is_some()
        );
        state.previews.clear();
        state.search_previews[0].poster = Some("fresh".into());
        state.artwork_recovery.clear();
        assert!(
            grid_artwork_recovery_target(&mut state, "series", "provider:show", "broken").is_none()
        );
    }

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
            grid_artwork_recovery_target(&mut state, "series", "native:old-show", &old),
            Some(("series".into(), "native:old-show".into()))
        );
        // Library and Home can fail together; share one metadata request.
        assert!(
            grid_artwork_recovery_target(&mut state, "series", "native:old-show", &old).is_none()
        );
        // A temporary offline failure can retry on a later page refresh.
        state.artwork_recovery.values_mut().for_each(|last| {
            *last -= Duration::from_secs(61);
        });
        assert!(
            grid_artwork_recovery_target(&mut state, "series", "native:old-show", &old).is_some()
        );
        state.entries[0].poster_url = "https://new.example/poster.jpg".into();
        assert!(
            grid_artwork_recovery_target(&mut state, "series", "native:old-show", &old).is_none()
        );
        assert!(grid_artwork_recovery_target(&mut state, "series", "removed", &old).is_none());
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

/// An opaque supplied logo would create a rectangular background in the hero.
/// Keep the readable text fallback when the source has no transparent pixels.
pub(super) fn transparent_title_logo(
    pixels: Option<SharedPixelBuffer<Rgba8Pixel>>,
) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    pixels.filter(|pixels| {
        pixels
            .as_bytes()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] < 255)
    })
}

#[cfg(test)]
mod title_logo_tests {
    use super::*;

    #[test]
    fn only_transparent_art_replaces_the_text_title() {
        let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(2, 1);
        pixels.make_mut_bytes().fill(255);
        assert!(transparent_title_logo(Some(pixels.clone())).is_none());
        pixels.make_mut_bytes()[3] = 0;
        assert!(transparent_title_logo(Some(pixels)).is_some());
        assert!(transparent_title_logo(None).is_none());
    }
}

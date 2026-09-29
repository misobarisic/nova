//! Discover catalog: fetching, pagination, prefetching and pickers.
use super::*;

impl Bridge {
    pub(super) fn load_catalog(&self) {
        // Reset pagination state for a fresh catalog selection.
        {
            let mut state = self.shared.lock().unwrap();
            state.next_skip = 0;
            state.catalog_exhausted = false;
            state.loading_more = false;
        }
        if let Some(app) = self.app() {
            app.set_loading_more(false);
        }
        self.fetch_catalog_page(false);
    }

    /// Endless-scroll: fetch the next page of the current catalog.
    /// Unload card posters outside the keep window (Discover, or Library
    /// when `library`), and (pre)fetch unloaded ones inside the reported
    /// fetch window — which the Slint grids already expand to visible ±2
    /// rows, i.e. one or two rows above/below the viewport.
    /// Keeps marathon endless-scroll sessions flat: card *metadata* stays
    /// (tiny), only the posters (CPU pixels + GPU textures) cycle. Missing
    /// posters reload from the disk / browser cache, not the network.
    ///
    /// The keep window is wider than the fetch window by
    /// `UNLOAD_EXTRA_CARDS` on each side (hysteresis), so preloaded rows
    /// are not blanked the moment the viewport jitters. Fetches run
    /// nearest-to-viewport-center first so the closest preload wins.
    pub(super) fn visible_range(&self, library: bool, first: i32, last: i32) {
        let app = match self.app() {
            Some(a) => a,
            None => return,
        };
        let model = if library { app.get_library() } else { app.get_catalog() };
        let len = model.row_count();
        if len == 0 {
            return;
        }
        let f = first.max(0) as usize;
        let l = (last.max(0) as usize).min(len.saturating_sub(1));
        if f > l {
            return;
        }
        let keep_f = f.saturating_sub(UNLOAD_EXTRA_CARDS);
        let keep_l = (l + UNLOAD_EXTRA_CARDS).min(len.saturating_sub(1));
        let mut blank: Vec<usize> = Vec::new();
        let mut fetch: Vec<(usize, String)> = Vec::new();
        for i in 0..len {
            let row = model.row_data(i);
            let loaded = row.as_ref().map(|c| c.is_loaded).unwrap_or(false);
            if i < keep_f || i > keep_l {
                if loaded {
                    blank.push(i);
                }
            } else if i >= f && i <= l && !loaded {
                if let Some(card) = row
                    && !card.poster_path.is_empty()
                {
                    fetch.push((i, card.poster_path.to_string()));
                }
            }
        }
        let unloaded = !blank.is_empty();
        for i in blank {
            if library {
                app.invoke_clear_library_poster(i as i32, Image::default());
            } else {
                app.invoke_clear_card_poster(i as i32, Image::default());
            }
        }
        // Nearest-to-center first: the row the user is most likely to scroll
        // into next decodes first (matters on desktop where far-away jobs
        // may already fill the background queue, and on Android where the
        // browser caps parallel fetches).
        if !fetch.is_empty() {
            let center = f + (l - f) / 2;
            fetch.sort_by_key(|(i, _)| i.abs_diff(center));
        }
        for (i, url) in fetch {
            self.fetch_card_poster(library, i, url);
        }
        // Unloaded images were just dropped — trim on throttled scroll
        // jumps only (the Slint side already gates frequency).
        if unloaded {
            trim_heap();
        }
    }

    pub(super) fn load_more(&self) {
        let (skip, supports_skip, exhausted) = {
            let state = self.shared.lock().unwrap();
            (state.next_skip, {
                let ty = state.type_defs.get(state.chosen_type);
                let cat = ty.and_then(|t| t.catalogs.get(state.chosen_catalog));
                cat.map_or(false, |c| c.supports_skip)
            }, state.catalog_exhausted)
        };
        if !supports_skip || exhausted || skip == 0 {
            return;
        }
        // loading_more is set synchronously below; the async fetch clears it
        // on completion, so duplicate calls while in-flight are no-ops.
        {
            let mut state = self.shared.lock().unwrap();
            if state.loading_more {
                return;
            }
            state.loading_more = true;
        }
        if let Some(app) = self.app() {
            app.set_loading_more(true);
        }
        self.fetch_catalog_page(true);
    }

    /// Core fetch: builds URLs (with `skip` extra when appending), fetches
    /// from all matching addons in parallel, and applies the merged result.
    pub(super) fn fetch_catalog_page(&self, append: bool) {
        let (urls, type_, catalog_id, supports_search, search, skip, generation) = {
            let state = self.shared.lock().unwrap();
            if state.chosen_type >= state.type_defs.len() {
                return;
            }
            let ty = &state.type_defs[state.chosen_type];
            if state.chosen_catalog >= ty.catalogs.len() {
                return;
            }
            let cat = &ty.catalogs[state.chosen_catalog];

            // Determine which addon URL(s) to fetch from.
            let urls: Vec<String> = if state.chosen_addon == usize::MAX {
                // "All addons" — find the addon that owns this catalog.
                state
                    .installed
                    .iter()
                    .filter(|a| a.enabled && a.label == cat.addon_label)
                    .map(|a| a.url.clone())
                    .collect()
            } else if state.chosen_addon < state.installed.len() {
                vec![state.installed[state.chosen_addon].url.clone()]
            } else {
                return;
            };

            let generation = if append {
                self.catalog_gen.load(Ordering::Relaxed)
            } else {
                let new_gen = self.catalog_gen.fetch_add(1, Ordering::Relaxed) + 1;
                self.catalog_gen.store(new_gen, Ordering::Relaxed);
                new_gen
            };

            (
                urls,
                ty.type_.clone(),
                cat.id.clone(),
                cat.supports_search,
                state.search.clone(),
                if append { state.next_skip } else { 0 },
                generation,
            )
        };

        // Build the endpoint URLs on the main thread (pure URL math), then
        // fetch from all matching addons in parallel and merge.
        let fetch_urls: Vec<String> = urls
            .iter()
            .filter_map(|url| {
                let addon = Addon::new(url).ok()?;
                let mut extra: Vec<(&str, String)> = if !search.is_empty() && supports_search {
                    vec![("search", search.clone())]
                } else {
                    vec![]
                };
                if append && skip > 0 {
                    extra.push(("skip", skip.to_string()));
                }
                let extra_refs: Vec<(&str, &str)> =
                    extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
                Some(addon.catalog_url(&type_, &catalog_id, &extra_refs))
            })
            .collect();

        let bridge = self.clone();
        crate::web_log(&format!(
            "nova: loading catalog (skip={skip}, append={append}) from {} url(s): {:?}",
            fetch_urls.len(),
            fetch_urls
        ));
        if fetch_urls.is_empty() {
            let bridge2 = bridge.clone();
            let _ = slint::invoke_from_event_loop(move || {
                bridge2.apply_catalog(generation, Vec::new(), append);
            });
            return;
        }

        let results = Arc::new(Mutex::new(Vec::<MetaPreview>::new()));
        let remaining = Arc::new(AtomicUsize::new(fetch_urls.len()));
        for url in fetch_urls {
            let bridge = bridge.clone();
            let results = results.clone();
            let remaining = remaining.clone();
            net::fetch_bytes(url, move |result| {
                if let Ok(bytes) = result
                    && let Ok(metas) = Addon::parse_catalog(&bytes)
                {
                    results.lock().unwrap().extend(metas);
                }
                // Last continuation home merges onto the UI thread.
                if remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
                    let all = std::mem::take(&mut *results.lock().unwrap());
                    let bridge2 = bridge.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        bridge2.apply_catalog(generation, all, append);
                    });
                }
            });
        }
    }

    /// Main thread: replace or append the grid with freshly fetched metas.
    pub(super) fn apply_catalog(&self, generation: u64, metas: Vec<MetaPreview>, append: bool) {
        let current = self.catalog_gen.load(Ordering::Relaxed);
        if generation != current {
            return; // stale response for an older selection
        }
        let app = match self.app() {
            Some(a) => a,
            None => return,
        };

        {
            let mut state = self.shared.lock().unwrap();
            let mut new_metas = metas;
            if append {
                // Dedup new items against existing previews by id.
                new_metas.retain(|m| !state.previews.iter().any(|p| p.id == m.id));
                if new_metas.is_empty() {
                    state.catalog_exhausted = true;
                    state.loading_more = false;
                    drop(state);
                    if let Some(app) = self.app() {
                        app.set_loading_more(false);
                    }
                    return;
                }
                state.next_skip += new_metas.len();
                let offset = state.previews.len();
                state.previews.extend(new_metas.iter().cloned());
                drop(state);

                // Push new cards onto the existing VecModel.
                let catalog = app.get_catalog();
                if let Some(vm) = catalog.as_any().downcast_ref::<VecModel<MediaCard>>() {
                    for m in &new_metas {
                        vm.push(MediaCard {
                            id: SharedString::from(&m.id),
                            title: SharedString::from(&m.title()),
                            year: SharedString::from(m.year_str().unwrap_or_default()),
                            poster_path: SharedString::from(m.poster.clone().unwrap_or_default()),
                            poster: Image::default(),
                            is_loaded: false,
                            badge: SharedString::default(),
                            watched: false,
                        });
                    }
                    // Poster downloads only for the new cards (background sweep:
                    // low priority — scroll-driven preloads jump ahead of it).
                    #[cfg(feature = "desktop")]
                    for (i, m) in new_metas.iter().enumerate() {
                        if let Some(poster) = &m.poster {
                            self.poster_tx.send_lo(PosterJob {
                                generation,
                                index: offset + i,
                                url: poster.clone(),
                                library: false,
                            });
                        }
                    }
                    #[cfg(not(feature = "desktop"))]
                    {
                        let app_weak = self.app.clone();
                        let gen_counter = self.catalog_gen.clone();
                        for (i, m) in new_metas.iter().enumerate() {
                            let Some(poster) = &m.poster else { continue };
                            let url = poster.clone();
                            let weak = app_weak.clone();
                            let gen_counter = gen_counter.clone();
                            net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
                                let Some(pixels) = pixels else { return };
                                let _ = slint::invoke_from_event_loop(move || {
                                    if generation != gen_counter.load(Ordering::Relaxed) {
                                        return;
                                    }
                                    if let Some(app) = weak.upgrade() {
                                        let img = Image::from_rgba8(pixels);
                                        app.invoke_set_card_poster(
                                            (offset + i) as i32,
                                            img.clone(),
                                        );
                                    }
                                });
                            });
                        }
                    }
                }
                let total = {
                    let state = self.shared.lock().unwrap();
                    state.previews.len()
                };
                let old_loading = {
                    let mut state = self.shared.lock().unwrap();
                    state.loading_more = false;
                    true
                };
                let _ = old_loading;
                app.set_loading_more(false);
                // Page settled: worker decode transients are freed — hand
                // fully-free pages back before the next scroll burst.
                trim_heap();
                crate::web_log(&format!(
                    "nova: catalog appended {} items (total {})",
                    new_metas.len(),
                    total
                ));
                #[cfg(feature = "desktop")]
                {
                    let cached = self.poster_cache.lock().unwrap().len();
                    crate::web_log(&format!(
                        "nova: memory posters-cached={cached} cards={total}"
                    ));
                }
            } else {
                // Replace mode (original behaviour).
                let cards: Vec<MediaCard> = new_metas
                    .iter()
                    .map(|m| MediaCard {
                        id: SharedString::from(&m.id),
                        title: SharedString::from(&m.title()),
                        year: SharedString::from(m.year_str().unwrap_or_default()),
                        poster_path: SharedString::from(m.poster.clone().unwrap_or_default()),
                        poster: Image::default(),
                        is_loaded: false,
                        badge: SharedString::default(),
                        watched: false,
                    })
                    .collect();

                state.next_skip = new_metas.len();
                state.catalog_exhausted = new_metas.is_empty();
                state.previews = new_metas.clone();
                state.catalog_gen = generation;
                drop(state);

                app.set_catalog(Rc::new(VecModel::from(cards)).into());
                app.set_empty_hint(SharedString::from(text::tr("No results for this selection.")));
                crate::web_log(&format!(
                    "nova: catalog applied ({} items)",
                    new_metas.len()
                ));

                // Poster downloads for every card (background sweep: low
                // priority — the top rows still win because index order is
                // viewport order on a fresh catalog, and scroll preloads use
                // the high-priority channel).
                #[cfg(feature = "desktop")]
                for (index, m) in new_metas.iter().enumerate() {
                    if let Some(poster) = &m.poster {
                        self.poster_tx.send_lo(PosterJob {
                            generation,
                            index,
                            url: poster.clone(),
                            library: false,
                        });
                    }
                }

                #[cfg(not(feature = "desktop"))]
                {
                    let app_weak = self.app.clone();
                    let gen_counter = self.catalog_gen.clone();
                    for (index, m) in new_metas.iter().enumerate() {
                        let Some(poster) = &m.poster else { continue };
                        let url = poster.clone();
                        let weak = app_weak.clone();
                        let gen_counter = gen_counter.clone();
                        net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
                            let Some(pixels) = pixels else { return };
                            let _ = slint::invoke_from_event_loop(move || {
                                if generation != gen_counter.load(Ordering::Relaxed) {
                                    return;
                                }
                                if let Some(app) = weak.upgrade() {
                                    let img = Image::from_rgba8(pixels);
                                    app.invoke_set_card_poster(index as i32, img.clone());
                                    if app.get_modal_visible()
                                        && app.get_selected_index() as usize == index
                                    {
                                        app.set_selected_poster(img);
                                    }
                                }
                            });
                        });
                    }
                }

                if active_cache_settings().prefetch_metadata {
                    self.prefetch_background_meta(&new_metas, generation);
                }
            }
        }

        // Update can_load_more: depends on the selected catalog's supports_skip
        // and whether we've exhausted the catalog.
        if let Some(app) = self.app() {
            let state = self.shared.lock().unwrap();
            let ty = state.type_defs.get(state.chosen_type);
            let cat = ty.and_then(|t| t.catalogs.get(state.chosen_catalog));
            let supports_skip = cat.map_or(false, |c| c.supports_skip);
            app.set_can_load_more(supports_skip && !state.catalog_exhausted);
        }
    }

    /// Prefetch season/episode metadata for catalog items in the background.
    /// Results are written to the disk cache so `prepare_episodes` finds them
    /// instantly when the user opens a series.
    pub(super) fn prefetch_background_meta(&self, metas: &[MetaPreview], _generation: u64) {
        // Resolve the current type label once.
        let current_type = {
            let state = self.shared.lock().unwrap();
            state
                .type_defs
                .get(state.chosen_type)
                .map(|td| td.type_.clone())
                .unwrap_or_default()
        };
        eprintln!(
            "prefetch: current_type={:?}, chosen_type={}, type_defs_len={}",
            current_type,
            {
                let state = self.shared.lock().unwrap();
                state.chosen_type
            },
            {
                let state = self.shared.lock().unwrap();
                state.type_defs.len()
            }
        );

        // Collect non-movie items missing episodes or header text.
        // Episodes alone aren't enough: without cached description/genres
        // the detail open still needs a network round-trip for pills +
        // synopsis, so those items are prefetched again for the header.
        let candidates: Vec<(String, String)> = metas
            .iter()
            .filter_map(|m| {
                let t = if m.type_.is_empty() { &current_type } else { &m.type_ };
                if t == "movie" || m.id.is_empty() {
                    return None;
                }
                if read_episodes_cache_for(t, &m.id).is_some() && header_text_cached(t, &m.id)
                {
                    return None;
                }
                Some((t.clone(), m.id.clone()))
            })
            .collect();

        eprintln!(
            "prefetch: {} candidates from {} metas (type_.empty={}, first_type={:?})",
            candidates.len(),
            metas.len(),
            metas.first().map_or(true, |m| m.type_.is_empty()),
            metas.first().map(|m| &m.type_),
        );

        if candidates.is_empty() {
            return;
        }

        self.prefetch_meta_pairs(candidates);
    }

    /// Prefetch episode metadata for a list of `(type_, id)` series in the
    /// background, writing results to the disk cache so `prepare_episodes`
    /// renders instantly. Shared by the catalog grid and the library page.
    pub(super) fn prefetch_meta_pairs(&self, pairs: Vec<(String, String)>) {
        if pairs.is_empty() {
            return;
        }
        // Early-out: need at least one addon with meta support.
        let installed: Vec<Installed> = {
            let state = self.shared.lock().unwrap();
            state
                .installed
                .iter()
                .filter(|a| a.enabled && a.manifest.has_meta())
                .cloned()
                .collect()
        };
        if installed.is_empty() {
            eprintln!("prefetch: no addons with meta support, skipping");
            return;
        }
        eprintln!(
            "prefetch: {} addon(s) with meta support",
            installed.len()
        );

        // Work items: one (type_, id) pair with the meta URLs of every
        // meta-capable addon, tried in order. Fetched sequentially via
        // fetch-continuations.
        let work: Vec<(String, String, Vec<String>)> = pairs
            .iter()
            .map(|(type_, id)| {
                let urls: Vec<String> = installed
                    .iter()
                    .filter_map(|cand| Addon::new(&cand.url).ok())
                    .map(|a| a.meta_url(type_, id))
                    .collect();
                (type_.clone(), id.clone(), urls)
            })
            .collect();

        let bridge = self.clone();
        fn run(
            bridge: Bridge,
            mut work: VecDeque<(String, String, Vec<String>)>,
            mut cached: u32,
        ) {
            // Next pair?
            let Some((type_, id, mut urls)) = work.pop_front() else {
                let msg = text::prefetch_done(cached as usize);
                eprintln!("prefetch: {msg}");
                let _ = slint::invoke_from_event_loop(move || {
                    // Newly cached episode lists feed badges/checks/Home.
                    bridge.refresh_library_progress_ui();
                });
                return;
            };
            // Skip pairs whose modal was opened while prefetching.
            {
                let state = bridge.shared.lock().unwrap();
                if state
                    .modal_item
                    .as_ref()
                    .map(|m| m.id == id)
                    .unwrap_or(false)
                {
                    drop(state);
                    run(bridge, work, cached);
                    return;
                }
            }
            if urls.is_empty() {
                eprintln!("prefetch: no addon provided episodes for {type_}/{id}");
                run(bridge, work, cached);
                return;
            }
            let url = urls.remove(0);
            let bridge2 = bridge.clone();
            net::fetch_bytes(url, move |result| {
                let mut pair_done = false;
                match result {
                    Err(e) => {
                        eprintln!(
                            "prefetch: meta fetch failed for {type_}/{id}: {}",
                            e.to_message()
                        );
                    }
                    Ok(bytes) => match Addon::parse_meta(&bytes) {
                        Ok(Some(item)) => {
                            // Cache the header (background/description/genres/
                            // year) alongside episodes: the prefetch used to
                            // discard it, so pills + synopsis still needed a
                            // network round-trip on open.
                            let header = meta_header_from_item(&item);
                            if merge_meta_header_for(&type_, &id, &header) {
                                eprintln!("prefetch: cached header for {type_}/{id}");
                            }
                            // Backfill saved library entries so Library
                            // reopens (and restarts) paint instantly.
                            if !header.background_url.is_empty() {
                                bridge2.persist_backdrop_for(&id, &header.background_url);
                            }
                            bridge2.persist_header_for(
                                &id,
                                &header.genres,
                                &header.description,
                                &header.year,
                                false,
                            );
                            let videos: Vec<Video> = item
                                .videos
                                .into_iter()
                                .filter(|v| v.season.is_some())
                                .collect();
                            if !videos.is_empty() {
                                eprintln!(
                                    "prefetch: cached {} episodes for {type_}/{id}",
                                    videos.len()
                                );
                                write_episodes_cache_for(&type_, &id, &videos);
                                cached += 1;
                                pair_done = true;
                            }
                        }
                        Ok(None) => {
                            eprintln!("prefetch: addon returned None for {type_}/{id}");
                        }
                        Err(e) => {
                            eprintln!("prefetch: meta parse failed for {type_}/{id}: {e}");
                        }
                    },
                }
                if pair_done {
                    run(bridge2, work, cached);
                } else {
                    // Try the next addon URL for this pair.
                    work.push_front((type_, id, urls));
                    run(bridge2, work, cached);
                }
            });
        }

        run(bridge, work.into(), 0);
    }

    /// Prefetch episode metadata for saved library series/anime entries, so
    /// opening them from My Library is instant. Runs when the library page is
    /// shown (and the prefetch setting is on); already-cached items are
    /// skipped. Library entries store their own `type_`, so no catalog-type
    /// resolution is needed here.
    pub(super) fn prefetch_library_meta(&self) {
        let entries = {
            let state = self.shared.lock().unwrap();
            state.entries.clone()
        };
        let pairs: Vec<(String, String)> = entries
            .iter()
            .filter_map(|e| {
                if e.id.is_empty() || e.type_.is_empty() || e.type_ == "movie" {
                    return None;
                }
                // Skip only when episodes are cached and header text is
                // available (persisted on the entry or in the header cache).
                // Otherwise the open still needs a meta round-trip for pills
                // + synopsis.
                let entry_has_text = !e.description.is_empty() || !e.genres.is_empty();
                if read_episodes_cache_for(&e.type_, &e.id).is_some()
                    && (entry_has_text || header_text_cached(&e.type_, &e.id))
                {
                    return None;
                }
                Some((e.type_.clone(), e.id.clone()))
            })
            .collect();
        self.prefetch_meta_pairs(pairs);
    }

    pub(super) fn pick_addon(&self, label: &str) {
        // The picker model is (re)built on catalog refreshes, so it can still
        // hold the source name from before a language switch: accept both.
        let idx = if label == text::tr("All addons") || label == "All addons" {
            usize::MAX
        } else {
            let state = self.shared.lock().unwrap();
            state
                .installed
                .iter()
                .position(|a| a.label == label)
                .unwrap_or(0)
        };
        {
            let mut state = self.shared.lock().unwrap();
            state.chosen_addon = idx;
        }
        self.refresh_all(true);
    }

    pub(super) fn pick_type(&self, label: &str) {
        let (changed, idx) = {
            let state = self.shared.lock().unwrap();
            let idx = state
                .type_defs
                .iter()
                .position(|t| t.label == label)
                .unwrap_or(0);
            (idx != state.chosen_type, idx)
        };
        {
            let mut state = self.shared.lock().unwrap();
            state.chosen_type = idx;
            state.chosen_catalog = 0;
        }
        if changed {
            self.apply_selection_to_ui();
            self.load_catalog();
        }
    }

    pub(super) fn pick_catalog(&self, label: &str) {
        let (changed, idx) = {
            let state = self.shared.lock().unwrap();
            let current_type = state
                .type_defs
                .get(state.chosen_type)
                .map(|t| t.catalogs.len())
                .unwrap_or(0);
            let idx = state
                .type_defs
                .get(state.chosen_type)
                .and_then(|t| t.catalogs.iter().position(|c| c.label == label))
                .unwrap_or(0);
            (idx != state.chosen_catalog || current_type == 0, idx)
        };
        {
            let mut state = self.shared.lock().unwrap();
            state.chosen_catalog = idx;
        }
        if changed {
            // Only update the combo index — the model (catalog_names) hasn't
            // changed, so we must not replace it or the ComboBox loses its
            // visual selection. The search-support UI still follows the new
            // catalog (hint + field enabled state).
            if let Some(app) = self.app() {
                app.set_catalog_combo_idx(idx as i32);
            }
            self.apply_search_support_to_ui();
            self.load_catalog();
        }
    }

    pub(super) fn submit_search(&self, text: &str) {
        let trimmed = text.trim().to_string();
        // The field + button are disabled for catalogs without search
        // support, but guard the backend path too: never fetch the plain
        // catalog instead of the requested query.
        if !trimmed.is_empty() && !self.catalog_supports_search() {
            return;
        }
        {
            let mut state = self.shared.lock().unwrap();
            state.search = trimmed.clone();
        }
        if let Some(app) = self.app() {
            app.set_search_text(SharedString::from(&trimmed));
        }
        self.load_catalog();
    }

    /// Refresh just the catalog combo (after type/catalog index changes).
    pub(super) fn apply_selection_to_ui(&self) {
        let app = match self.app() {
            Some(a) => a,
            None => return,
        };
        let (type_idx, catalog_idx, catalog_names, hint, searchable, has_grid, supports_skip) = {
            let state = self.shared.lock().unwrap();
            let type_idx = state.chosen_type;
            let catalog_idx = state.chosen_catalog.min(
                state
                    .type_defs
                    .get(state.chosen_type)
                    .map(|t| t.catalogs.len().saturating_sub(1))
                    .unwrap_or(0),
            );
            let names: Vec<SharedString> = state
                .type_defs
                .get(state.chosen_type)
                .map(|t| {
                    t.catalogs
                        .iter()
                        .map(|c| SharedString::from(&c.label))
                        .collect()
                })
                .unwrap_or_default();
            // Support flags follow the *chosen* catalog, not the first one:
            // with mixed catalogs the hint/enabled state must describe what
            // search + pagination will actually do.
            let (hint, searchable, supports_skip) = state
                .type_defs
                .get(state.chosen_type)
                .and_then(|t| t.catalogs.get(catalog_idx))
                .map(|c| {
                    let hint = if c.supports_search {
                        text::tr("Search (e.g. a movie title)").to_string()
                    } else {
                        text::tr("This catalog has no search").to_string()
                    };
                    (hint, c.supports_search, c.supports_skip)
                })
                .unwrap_or_default();
            (
                type_idx,
                catalog_idx,
                names,
                hint,
                searchable,
                !state.type_defs.is_empty(),
                supports_skip,
            )
        };
        app.set_catalog_names(Rc::new(VecModel::from(catalog_names)).into());
        app.set_type_combo_idx(type_idx as i32);
        app.set_catalog_combo_idx(catalog_idx as i32);
        if has_grid {
            app.set_searchable_hint(SharedString::from(hint));
            app.set_searchable(searchable);
            // can_load_more is set after fetch completes; for now only mark
            // it available when the catalog supports skip.
            app.set_can_load_more(supports_skip);
        } else {
            app.set_searchable_hint(SharedString::from(""));
            app.set_searchable(false);
            app.set_can_load_more(false);
        }
    }

    /// Refresh the search-support UI (hint + field enabled state) for the
    /// currently chosen catalog without touching the combo models — used
    /// when the catalog changes but the model hasn't, so the ComboBox keeps
    /// its visual selection (see `pick_catalog`), and after addon
    /// installs/toggles rebuild the catalogs (`refresh_all`).
    pub(super) fn apply_search_support_to_ui(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let (hint, searchable) = {
            let state = self.shared.lock().unwrap();
            state
                .type_defs
                .get(state.chosen_type)
                .and_then(|t| t.catalogs.get(state.chosen_catalog))
                .map(|c| {
                    let hint = if c.supports_search {
                        "Search (e.g. a movie title)".to_string()
                    } else {
                        "This catalog has no search".to_string()
                    };
                    (hint, c.supports_search)
                })
                .unwrap_or((String::new(), false))
        };
        app.set_searchable_hint(SharedString::from(hint));
        app.set_searchable(searchable);
    }

    /// Whether the currently chosen catalog accepts a search query.
    pub(super) fn catalog_supports_search(&self) -> bool {
        let state = self.shared.lock().unwrap();
        state
            .type_defs
            .get(state.chosen_type)
            .and_then(|t| t.catalogs.get(state.chosen_catalog))
            .map(|c| c.supports_search)
            .unwrap_or(false)
    }

}

pub(crate) fn build_type_defs(manifest: &Manifest, addon_label: &str) -> Vec<TypeDef> {
    use std::collections::HashMap;

    let mut by_type: HashMap<String, Vec<&addons::Catalog>> = HashMap::new();
    for cat in &manifest.catalogs {
        by_type.entry(cat.type_.clone()).or_default().push(cat);
    }

    // Order: types as declared by the manifest first, then any extra catalog
    // types, but only types that actually have catalogs (grid needs them).
    let mut ordered: Vec<String> = Vec::new();
    for t in &manifest.types {
        if by_type.contains_key(t) && !ordered.contains(t) {
            ordered.push(t.clone());
        }
    }
    for t in by_type.keys() {
        if !ordered.contains(t) {
            ordered.push(t.clone());
        }
    }

    ordered
        .into_iter()
        .filter_map(|type_| {
            let cats = by_type.get(&type_)?;
            // Disambiguate catalog labels (a type could declare two catalogs
            // with the same display name).
            let mut counts: HashMap<String, usize> = HashMap::new();
            for c in cats {
                *counts.entry(c.name.clone()).or_insert(0) += 1;
            }
            let catalogs: Vec<CatDef> = cats
                .iter()
                .map(|c| {
                    let label = if counts[&c.name] > 1 {
                        format!("{} ({})", c.name, c.id)
                    } else {
                        c.name.clone()
                    };
                    CatDef {
                        id: c.id.clone(),
                        supports_search: c.supports_extra("search"),
                        supports_skip: c.supports_extra("skip"),
                        label,
                        addon_label: addon_label.to_string(),
                    }
                })
                .collect();
            Some(TypeDef {
                type_: type_.clone(),
                label: type_.clone(),
                catalogs,
            })
        })
        .collect()
}
/// Merge type_defs from the enabled addons into a single list.
/// Catalog labels are prefixed with the addon name for disambiguation.
/// Disabled addons are skipped so they never feed Discover.
pub(crate) fn build_merged_type_defs(installed: &[Installed]) -> Vec<TypeDef> {
    use std::collections::HashMap;

    let active: Vec<&Installed> = installed.iter().filter(|a| a.enabled).collect();

    // Collect all (type, catalog, addon_label) triples.
    let mut by_type: HashMap<String, Vec<(String, String, bool, bool, String)>> = HashMap::new();
    for inst in &active {
        for cat in &inst.manifest.catalogs {
            by_type
                .entry(cat.type_.clone())
                .or_default()
                .push((
                    cat.id.clone(),
                    cat.name.clone(),
                    cat.supports_extra("search"),
                    cat.supports_extra("skip"),
                    inst.label.clone(),
                ));
        }
    }

    // Stable type order: types from the first addon first, then extras.
    let mut ordered: Vec<String> = Vec::new();
    for inst in &active {
        for t in &inst.manifest.types {
            if by_type.contains_key(t) && !ordered.contains(t) {
                ordered.push(t.clone());
            }
        }
    }
    for t in by_type.keys() {
        if !ordered.contains(t) {
            ordered.push(t.clone());
        }
    }

    ordered
        .into_iter()
        .filter_map(|type_| {
            let cats = by_type.get(&type_)?;
            let catalogs: Vec<CatDef> = cats
                .iter()
                .map(|(id, name, search, skip, addon)| {
                    let label = format!("{addon} — {name}");
                    CatDef {
                        id: id.clone(),
                        supports_search: *search,
                        supports_skip: *skip,
                        label,
                        addon_label: addon.clone(),
                    }
                })
                .collect();
            Some(TypeDef {
                type_: type_.clone(),
                label: type_.clone(),
                catalogs,
            })
        })
        .collect()
}

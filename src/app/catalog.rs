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
        let searching = !library && app.get_discover_search_open();
        let model = if library {
            app.get_library()
        } else if searching {
            app.get_search_results()
        } else {
            app.get_catalog()
        };
        let search_previews =
            searching.then(|| self.shared.lock().unwrap().search_previews.clone());
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
            } else if i >= f
                && i <= l
                && !loaded
                && let Some(card) = row
                && !card.poster_path.is_empty()
            {
                fetch.push((i, card.poster_path.to_string()));
            }
        }
        let unloaded = !blank.is_empty();
        for i in blank {
            if library {
                app.invoke_clear_library_poster(i as i32, Image::default());
            } else if searching {
                app.invoke_clear_search_card_poster(i as i32, Image::default());
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
            if searching {
                let generation = self.shared.lock().unwrap().search_generation;
                if let Some(meta) = search_previews.as_ref().and_then(|metas| metas.get(i)) {
                    self.fetch_search_card_poster(
                        meta.type_.clone(),
                        meta.id.clone(),
                        url,
                        generation,
                    );
                }
            } else {
                self.fetch_card_poster(library, i, url);
            }
        }
        // Unloaded images were just dropped — trim on throttled scroll
        // jumps only (the Slint side already gates frequency).
        if unloaded {
            trim_heap();
        }
    }

    pub(super) fn load_more(&self) {
        if self.app().is_some_and(|app| app.get_discover_search_open()) {
            self.load_more_search();
            return;
        }
        let (skip, supports_skip, exhausted) = {
            let state = self.shared.lock().unwrap();
            (
                state.next_skip,
                {
                    let ty = state.type_defs.get(state.chosen_type);
                    let cat = ty.and_then(|t| t.catalogs.get(state.chosen_catalog));
                    cat.is_some_and(|c| c.supports_skip)
                },
                state.catalog_exhausted,
            )
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
        let (urls, type_, catalog_id, genre, skip, generation) = {
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
                state.chosen_genre.clone(),
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
                let mut extra: Vec<(&str, String)> = if genre.is_empty() {
                    vec![]
                } else {
                    vec![("genre", genre.clone())]
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
                app.set_empty_hint(SharedString::from(text::tr(
                    "No results for this selection.",
                )));
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
            let supports_skip = cat.is_some_and(|c| c.supports_skip);
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
                let t = if m.type_.is_empty() {
                    &current_type
                } else {
                    &m.type_
                };
                if t == "movie" || m.id.is_empty() {
                    return None;
                }
                if read_episodes_cache_for(t, &m.id).is_some() && header_text_cached(t, &m.id) {
                    return None;
                }
                Some((t.clone(), m.id.clone()))
            })
            .collect();

        eprintln!(
            "prefetch: {} candidates from {} metas (type_.empty={}, first_type={:?})",
            candidates.len(),
            metas.len(),
            metas.first().is_none_or(|m| m.type_.is_empty()),
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
        eprintln!("prefetch: {} addon(s) with meta support", installed.len());

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
        fn run(bridge: Bridge, mut work: VecDeque<(String, String, Vec<String>)>, mut cached: u32) {
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
            state.chosen_genre.clear();
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
            state.chosen_genre.clear();
        }
        if changed {
            // Only update the combo index — the model (catalog_names) hasn't
            // changed, so we must not replace it or the Dropdown loses its
            // visual selection. Genre options follow the newly chosen catalog;
            // global search availability is independent of this selection.
            if let Some(app) = self.app() {
                app.set_catalog_combo_idx(idx as i32);
            }
            self.apply_genre_selection_to_ui();
            self.apply_search_support_to_ui();
            self.load_catalog();
        }
    }

    pub(super) fn pick_genre(&self, label: &str) {
        let genre = if label == text::tr("All genres") || label == "All genres" {
            String::new()
        } else {
            label.to_string()
        };
        let changed = {
            let mut state = self.shared.lock().unwrap();
            let options = state
                .type_defs
                .get(state.chosen_type)
                .and_then(|ty| ty.catalogs.get(state.chosen_catalog))
                .map(|cat| &cat.genre_options);
            let valid = genre.is_empty() || options.is_some_and(|items| items.contains(&genre));
            if !valid || state.chosen_genre == genre {
                false
            } else {
                state.chosen_genre = genre;
                true
            }
        };
        if changed {
            self.apply_genre_selection_to_ui();
            self.load_catalog();
        }
    }

    /// Invalidate the previous query as soon as the user edits the field.
    /// Schedule the next request after a short quiet period; a late response
    /// from the old query must not repopulate results while the user types.
    pub(super) fn search_edited(&self, text: &str) {
        let has_query = text.trim().chars().count() >= 2;
        let app = self.app();
        let results_open = app
            .as_ref()
            .is_some_and(|app| app.get_discover_search_open());
        let keep_results_open = search_results_open_after_edit(text, results_open);
        let generation = {
            let mut state = self.shared.lock().unwrap();
            state.search_generation = state.search_generation.wrapping_add(1);
            state.search_targets.clear();
            state.search_previews.clear();
            state.search_poster_inflight.clear();
            state.search_loading_more = false;
            state.search_generation
        };

        if let Some(app) = app {
            app.set_discover_search_animate_results(true);
            app.set_search_results(Rc::new(VecModel::<MediaCard>::from(vec![])).into());
            app.set_search_loading(has_query);
            app.set_search_loading_more(false);
            app.set_search_can_load_more(false);
            let empty_hint = if has_query {
                text::tr("No results for this search.")
            } else {
                text::tr("Type at least 2 characters to search.")
            };
            app.set_search_empty_hint(SharedString::from(empty_hint));
            if has_query {
                app.set_discover_search_scroll_y(0.0);
                app.set_discover_kb_zone(2);
                app.set_discover_kb_ctl(3);
            }
            // Once results are open, keep them open while the user edits
            // below the minimum; Back is the explicit way out of search.
            app.set_discover_search_open(keep_results_open);
        }

        if has_query {
            let query = text.trim().to_string();
            let bridge = self.clone();
            slint::Timer::single_shot(Duration::from_millis(350), move || {
                let current = bridge.shared.lock().unwrap().search_generation == generation;
                if current {
                    bridge.submit_search(&query);
                }
            });
        }
    }

    pub(super) fn submit_search(&self, text: &str) {
        let trimmed = text.trim().to_string();
        if trimmed.chars().count() < 2 {
            self.search_edited(&trimmed);
            return;
        }

        let (generation, target_count) = {
            let mut state = self.shared.lock().unwrap();
            state.search_generation = state.search_generation.wrapping_add(1);
            let generation = state.search_generation;
            state.search = trimmed.clone();
            state.search_previews.clear();
            state.search_targets = build_search_targets(&state.installed);
            state.search_loading_more = false;
            state.search_poster_inflight.clear();
            (generation, state.search_targets.len())
        };

        if let Some(app) = self.app() {
            app.set_search_text(SharedString::from(&trimmed));
            app.set_discover_search_animate_results(true);
            app.set_search_results(Rc::new(VecModel::<MediaCard>::from(vec![])).into());
            app.set_discover_search_open(true);
            app.set_discover_kb_zone(2);
            app.set_discover_kb_ctl(3);
            app.set_search_loading(true);
            app.set_search_loading_more(false);
            app.set_search_can_load_more(false);
            app.set_search_empty_hint(SharedString::from(text::tr("No results for this search.")));
        }

        let targets = (0..target_count).collect();
        self.fetch_search_pages(generation, trimmed, targets, false);
    }

    /// Search every enabled addon catalog whose manifest advertises `search`.
    /// Browse selection is intentionally not modified by a query.
    fn fetch_search_pages(
        &self,
        generation: u64,
        query: String,
        target_indices: Vec<usize>,
        append: bool,
    ) {
        let work: Vec<(usize, String, String)> = {
            let state = self.shared.lock().unwrap();
            if generation != state.search_generation {
                return;
            }
            target_indices
                .into_iter()
                .filter_map(|index| {
                    let target = state.search_targets.get(index)?;
                    let addon = Addon::new(&target.addon_url).ok()?;
                    let mut extras = vec![("search", query.clone())];
                    if append && target.supports_skip && target.next_skip > 0 {
                        extras.push(("skip", target.next_skip.to_string()));
                    }
                    let refs: Vec<(&str, &str)> = extras
                        .iter()
                        .map(|(key, value)| (*key, value.as_str()))
                        .collect();
                    Some((
                        index,
                        addon.catalog_url(&target.type_, &target.catalog_id, &refs),
                        target.type_.clone(),
                    ))
                })
                .collect()
        };

        if work.is_empty() {
            let bridge = self.clone();
            let _ = slint::invoke_from_event_loop(move || {
                bridge.apply_search_results(generation, Vec::new(), append);
            });
            return;
        }

        let results = Arc::new(Mutex::new(Vec::<(usize, Vec<MetaPreview>)>::new()));
        let remaining = Arc::new(AtomicUsize::new(work.len()));
        for (target_index, url, type_) in work {
            let bridge = self.clone();
            let results = results.clone();
            let remaining = remaining.clone();
            net::fetch_bytes(url, move |result| {
                let mut metas = result
                    .ok()
                    .and_then(|bytes| Addon::parse_catalog(&bytes).ok())
                    .unwrap_or_default();
                for meta in &mut metas {
                    if meta.type_.is_empty() {
                        meta.type_ = type_.clone();
                    }
                }
                results.lock().unwrap().push((target_index, metas));
                if remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
                    let mut pages = std::mem::take(&mut *results.lock().unwrap());
                    pages.sort_by_key(|(index, _)| *index);
                    let _ = slint::invoke_from_event_loop(move || {
                        bridge.apply_search_results(generation, pages, append);
                    });
                }
            });
        }
    }

    /// Apply one fan-out page in manifest order, preserving the browse model
    /// and maintaining independent cursors for catalogs that support skip.
    fn apply_search_results(
        &self,
        generation: u64,
        pages: Vec<(usize, Vec<MetaPreview>)>,
        append: bool,
    ) {
        let Some(app) = self.app() else { return };
        let (old_previews, ranked_metas, new_count, can_load_more) = {
            let mut state = self.shared.lock().unwrap();
            if generation != state.search_generation {
                return;
            }
            let old_previews = state.search_previews.clone();

            for (index, metas) in &pages {
                if let Some(target) = state.search_targets.get_mut(*index) {
                    if metas.is_empty() || !target.supports_skip {
                        target.exhausted = true;
                    } else {
                        target.next_skip += metas.len();
                    }
                }
            }

            let mut seen: HashSet<(String, String)> = state
                .search_previews
                .iter()
                .map(|meta| (meta.type_.clone(), meta.id.clone()))
                .collect();
            let mut new_metas = Vec::new();
            for (_, metas) in pages {
                for meta in metas {
                    if !meta.id.is_empty() && seen.insert((meta.type_.clone(), meta.id.clone())) {
                        new_metas.push(meta);
                    }
                }
            }

            let new_count = new_metas.len();
            if append {
                state.search_previews.extend(new_metas.iter().cloned());
            } else {
                state.search_previews = new_metas.clone();
            }
            let query = state.search.clone();
            // Stable sorting leaves the existing add-on/catalog order intact
            // for equally relevant matches.
            state
                .search_previews
                .sort_by_cached_key(|meta| std::cmp::Reverse(search_relevance_score(&query, meta)));
            let ranked_metas = state.search_previews.clone();
            state.search_loading_more = false;
            let can_load_more = state
                .search_targets
                .iter()
                .any(|target| target.supports_skip && !target.exhausted);
            (old_previews, ranked_metas, new_count, can_load_more)
        };

        // Rebuild in relevance order while carrying already-decoded posters
        // across position changes. Poster completions are resolved by metadata
        // identity below, so an in-flight image remains attached to its item.
        let old_model = app.get_search_results();
        let mut old_cards = HashMap::new();
        for (index, meta) in old_previews.iter().enumerate() {
            if let Some(card) = old_model.row_data(index) {
                old_cards.insert((meta.type_.clone(), meta.id.clone()), card);
            }
        }

        let mut cards = Vec::with_capacity(ranked_metas.len());
        let mut poster_jobs = Vec::new();
        for meta in &ranked_metas {
            let key = (meta.type_.clone(), meta.id.clone());
            let card = old_cards
                .remove(&key)
                .unwrap_or_else(|| search_media_card(meta));
            if !card.is_loaded
                && let Some(poster) = &meta.poster
            {
                poster_jobs.push((meta.type_.clone(), meta.id.clone(), poster.clone()));
            }
            cards.push(card);
        }
        app.set_search_results(Rc::new(VecModel::from(cards)).into());

        for (type_, id, poster) in poster_jobs {
            self.fetch_search_card_poster(type_, id, poster, generation);
        }
        app.set_search_loading(false);
        app.set_search_loading_more(false);
        app.set_search_can_load_more(can_load_more);
        if !append {
            let query = self.shared.lock().unwrap().search.clone();
            self.remember_search(&query);
        }
        crate::web_log(&format!(
            "nova: search page applied ({} new items, append={append}, can_load_more={can_load_more})",
            new_count
        ));
    }

    pub(super) fn load_more_search(&self) {
        let (generation, query, targets) = {
            let mut state = self.shared.lock().unwrap();
            if state.search_loading_more {
                return;
            }
            let targets: Vec<usize> = state
                .search_targets
                .iter()
                .enumerate()
                .filter_map(|(index, target)| {
                    (target.supports_skip && !target.exhausted).then_some(index)
                })
                .collect();
            if targets.is_empty() {
                return;
            }
            state.search_loading_more = true;
            (state.search_generation, state.search.clone(), targets)
        };
        if let Some(app) = self.app() {
            app.set_search_loading_more(true);
        }
        self.fetch_search_pages(generation, query, targets, true);
    }

    pub(super) fn close_search_results(&self) {
        {
            let mut state = self.shared.lock().unwrap();
            state.search_generation = state.search_generation.wrapping_add(1);
            state.search.clear();
            state.search_targets.clear();
            state.search_poster_inflight.clear();
            state.search_loading_more = false;
        }
        if let Some(app) = self.app() {
            app.set_search_text(SharedString::default());
            app.set_search_loading(false);
            app.set_search_loading_more(false);
            app.set_search_can_load_more(false);
            app.set_discover_search_open(false);
        }
    }

    // Search history is deliberately local: it never enters the sync domains.
    pub(super) fn load_search_history(&self) {
        let stored = read_json::<Vec<String>>("discover:search_history").unwrap_or_default();
        let mut history = Vec::new();
        for query in stored.iter().rev() {
            remember_search_query(&mut history, query);
        }
        if let Some(app) = self.app() {
            app.set_discover_search_history(
                Rc::new(VecModel::from(
                    history
                        .into_iter()
                        .map(SharedString::from)
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
        }
    }

    fn remember_search(&self, query: &str) {
        let Some(app) = self.app() else {
            return;
        };
        let model = app.get_discover_search_history();
        let mut history: Vec<String> = model.iter().map(|value| value.to_string()).collect();
        remember_search_query(&mut history, query);
        write_json("discover:search_history", &history);
        app.set_discover_search_history(
            Rc::new(VecModel::from(
                history
                    .into_iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
    }

    pub(super) fn clear_search_history(&self) {
        write_json("discover:search_history", &Vec::<String>::new());
        if let Some(app) = self.app() {
            app.set_discover_search_history(Rc::new(VecModel::<SharedString>::default()).into());
        }
    }

    pub(super) fn remove_search_history_item(&self, query: &str) {
        let Some(app) = self.app() else {
            return;
        };
        let mut history: Vec<String> = app
            .get_discover_search_history()
            .iter()
            .map(|value| value.to_string())
            .collect();
        let previous_len = history.len();
        history.retain(|saved| saved != query);
        if history.len() == previous_len {
            return;
        }
        write_json("discover:search_history", &history);
        app.set_discover_search_history(
            Rc::new(VecModel::from(
                history
                    .into_iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
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
            // Search is global across all enabled addons, while pagination
            // for the browse grid still belongs to the selected catalog.
            let supports_search = state.installed.iter().any(|addon| {
                addon.enabled
                    && addon
                        .manifest
                        .catalogs
                        .iter()
                        .any(|c| c.supports_extra("search"))
            });
            let hint = if supports_search {
                text::tr("Search movies, shows…").to_string()
            } else {
                text::tr("No enabled catalogs support search.").to_string()
            };
            let supports_skip = state
                .type_defs
                .get(state.chosen_type)
                .and_then(|t| t.catalogs.get(catalog_idx))
                .map(|c| c.supports_skip)
                .unwrap_or(false);
            (
                type_idx,
                catalog_idx,
                names,
                hint,
                supports_search,
                !state.type_defs.is_empty(),
                supports_skip,
            )
        };
        app.set_catalog_names(Rc::new(VecModel::from(catalog_names)).into());
        self.apply_catalog_labels_to_ui();
        app.set_type_combo_idx(type_idx as i32);
        app.set_catalog_combo_idx(catalog_idx as i32);
        self.apply_genre_selection_to_ui();
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

    /// Build the genre filter from the selected catalog's declared `genre`
    /// extra. The empty option clears the filter and remains localized.
    pub(super) fn apply_genre_selection_to_ui(&self) {
        let Some(app) = self.app() else { return };
        let (mut names, selected) = {
            let state = self.shared.lock().unwrap();
            let options = state
                .type_defs
                .get(state.chosen_type)
                .and_then(|ty| ty.catalogs.get(state.chosen_catalog))
                .map(|cat| cat.genre_options.clone())
                .unwrap_or_default();
            let selected = state.chosen_genre.clone();
            (options, selected)
        };
        let index = if names.is_empty() {
            0
        } else {
            names.insert(0, text::tr("All genres").to_string());
            if selected.is_empty() {
                0
            } else {
                names
                    .iter()
                    .position(|genre| genre == &selected)
                    .unwrap_or(0)
            }
        };
        app.set_genre_names(
            Rc::new(VecModel::from(
                names
                    .into_iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        app.set_genre_combo_idx(index as i32);
    }

    /// Dropdown labels are separate from its values, so hiding source names
    /// cannot make two identically named catalogs select the wrong add-on.
    pub(super) fn apply_catalog_labels_to_ui(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let labels = {
            let state = self.shared.lock().unwrap();
            let hide_prefix = state.chosen_addon == usize::MAX
                && !state.cache_settings.discover_catalog_addon_names;
            state
                .type_defs
                .get(state.chosen_type)
                .map(|ty| {
                    ty.catalogs
                        .iter()
                        .map(|cat| SharedString::from(catalog_display_label(cat, hide_prefix)))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        app.set_catalog_labels(Rc::new(VecModel::from(labels)).into());
    }

    /// Refresh global-search availability without rebuilding picker models —
    /// used after addon installs/toggles.
    pub(super) fn apply_search_support_to_ui(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let (hint, searchable) = {
            let state = self.shared.lock().unwrap();
            let searchable = state.installed.iter().any(|addon| {
                addon.enabled
                    && addon
                        .manifest
                        .catalogs
                        .iter()
                        .any(|c| c.supports_extra("search"))
            });
            let hint = if searchable {
                text::tr("Search movies, shows…").to_string()
            } else {
                text::tr("No enabled catalogs support search.").to_string()
            };
            (hint, searchable)
        };
        app.set_searchable_hint(SharedString::from(hint));
        app.set_searchable(searchable);
    }

    /// Rebuild localized Discover labels without changing the active catalog,
    /// query, pagination, or search-result models.
    pub(super) fn refresh_catalog_language_text(&self) {
        let Some(app) = self.app() else { return };
        let (has_grid, searchable) = {
            let state = self.shared.lock().unwrap();
            let searchable = state.installed.iter().any(|addon| {
                addon.enabled
                    && addon
                        .manifest
                        .catalogs
                        .iter()
                        .any(|catalog| catalog.supports_extra("search"))
            });
            (!state.type_defs.is_empty(), searchable)
        };
        app.set_searchable(searchable && has_grid);
        app.set_searchable_hint(SharedString::from(if has_grid {
            if searchable {
                text::tr("Search movies, shows…")
            } else {
                text::tr("No enabled catalogs support search.")
            }
        } else {
            ""
        }));

        let query = app.get_search_text();
        let search_hint = if query.trim().chars().count() >= 2 {
            text::tr("No results for this search.")
        } else {
            text::tr("Type at least 2 characters to search.")
        };
        app.set_search_empty_hint(SharedString::from(search_hint));
        self.apply_catalog_labels_to_ui();
        self.apply_genre_selection_to_ui();
    }
}

fn build_search_targets(installed: &[Installed]) -> Vec<SearchTarget> {
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for addon in installed.iter().filter(|addon| addon.enabled) {
        if Addon::new(&addon.url).is_err() {
            continue;
        }
        for catalog in &addon.manifest.catalogs {
            if !catalog.supports_extra("search")
                || !seen.insert((addon.url.clone(), catalog.type_.clone(), catalog.id.clone()))
            {
                continue;
            }
            targets.push(SearchTarget {
                addon_url: addon.url.clone(),
                type_: catalog.type_.clone(),
                catalog_id: catalog.id.clone(),
                supports_skip: catalog.supports_extra("skip"),
                next_skip: 0,
                exhausted: false,
            });
        }
    }
    targets
}

fn search_results_open_after_edit(query: &str, was_open: bool) -> bool {
    was_open || query.trim().chars().count() >= 2
}

fn catalog_display_label(cat: &CatDef, hide_prefix: bool) -> &str {
    if hide_prefix {
        cat.label
            .strip_prefix(&format!("{} — ", cat.addon_label))
            .unwrap_or(&cat.label)
    } else {
        &cat.label
    }
}

fn remember_search_query(history: &mut Vec<String>, query: &str) {
    let query = query.trim();
    if !(2..=256).contains(&query.chars().count()) {
        return;
    }
    let key = query.to_lowercase();
    history.retain(|old| old.to_lowercase() != key);
    history.insert(0, query.to_string());
    history.truncate(20);
}

fn search_relevance_score(query: &str, meta: &MetaPreview) -> u32 {
    let title = meta.title();
    let year = meta.year_str();
    search_match_score(query, &title, year.as_deref())
}

/// Rank exact, prefix, phrase, token and typo-tolerant matches in descending
/// tiers. The gaps between tiers ensure a fuzzy result cannot outrank an exact
/// or strong word match just because it has a similar character count.
fn search_match_score(query: &str, title: &str, year: Option<&str>) -> u32 {
    let query = normalize_search_text(query);
    let title = normalize_search_text(title);
    if query.is_empty() || title.is_empty() {
        return 0;
    }

    let mut title_and_year = title.clone();
    if let Some(year) = year {
        let year = normalize_search_text(year);
        if !year.is_empty() {
            title_and_year.push(' ');
            title_and_year.push_str(&year);
        }
    }

    if query == title {
        return 1_000_000;
    }
    if query == title_and_year {
        return 990_000;
    }

    let suffix_len = title_and_year
        .chars()
        .count()
        .saturating_sub(query.chars().count());
    if title.starts_with(&query) || title_and_year.starts_with(&query) {
        return 900_000u32.saturating_sub(suffix_len.min(10_000) as u32);
    }

    let padded_title = format!(" {title_and_year} ");
    let padded_query = format!(" {query} ");
    if let Some(position) = padded_title.find(&padded_query) {
        return 820_000u32.saturating_sub(position.min(10_000) as u32 * 10);
    }

    let query_tokens: Vec<&str> = query.split_whitespace().collect();
    let title_tokens: Vec<&str> = title_and_year.split_whitespace().collect();
    if query_tokens.is_empty() || title_tokens.is_empty() {
        return 0;
    }

    let mut used_title_tokens = vec![false; title_tokens.len()];
    let mut matched_positions = Vec::new();
    let mut exact_hits = 0usize;
    let mut fuzzy_hits = 0usize;
    for query_token in &query_tokens {
        if let Some(index) = title_tokens
            .iter()
            .enumerate()
            .find_map(|(index, title_token)| {
                (!used_title_tokens[index] && title_token == query_token).then_some(index)
            })
        {
            used_title_tokens[index] = true;
            matched_positions.push(index);
            exact_hits += 1;
            continue;
        }

        // Exact numeric tokens (especially years) should not fuzzy-match a
        // neighboring number; typo tolerance is for title words.
        if query_token.chars().all(|character| character.is_numeric()) {
            continue;
        }
        let best = title_tokens
            .iter()
            .enumerate()
            .filter(|(index, _)| !used_title_tokens[*index])
            .map(|(index, title_token)| {
                (index, normalized_edit_similarity(query_token, title_token))
            })
            .max_by_key(|(_, similarity)| *similarity);
        if let Some((index, _similarity)) = best.filter(|(_, similarity)| *similarity >= 700) {
            used_title_tokens[index] = true;
            matched_positions.push(index);
            fuzzy_hits += 1;
        }
    }

    let in_order = matched_positions
        .windows(2)
        .filter(|pair| pair[0] < pair[1])
        .count();
    let matched = exact_hits + fuzzy_hits;
    if matched == query_tokens.len() {
        if fuzzy_hits == 0 {
            return 720_000 + in_order.min(20) as u32 * 1_000;
        }
        return 650_000 + in_order.min(20) as u32 * 1_000 + exact_hits.min(20) as u32 * 500;
    }
    if matched > 0 {
        return 350_000
            + (matched as u32 * 200_000 / query_tokens.len() as u32)
            + exact_hits.min(20) as u32 * 500;
    }

    // Compare the complete query against short title-word windows, so spacing
    // and small spelling differences (e.g. "spiderman" / "spider man") match.
    if query.chars().count() < 3 {
        return 0;
    }
    let min_window = query_tokens.len().saturating_sub(1).max(1);
    let max_window = (query_tokens.len() + 1).min(title_tokens.len());
    let mut best_similarity = 0;
    for width in min_window..=max_window {
        for start in 0..=title_tokens.len() - width {
            let window = title_tokens[start..start + width].join(" ");
            best_similarity = best_similarity.max(normalized_edit_similarity(&query, &window));
        }
    }
    if best_similarity >= 650 {
        100_000 + best_similarity * 100
    } else {
        0
    }
}

fn normalize_search_text(value: &str) -> String {
    let mut normalized = String::new();
    let mut pending_space = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            if pending_space && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push(character);
            pending_space = false;
        } else {
            pending_space = true;
        }
    }
    normalized
}

/// Similarity on a 0..=1000 scale, using Damerau-Levenshtein distance so an
/// adjacent transposition counts as one typo.
fn normalized_edit_similarity(left: &str, right: &str) -> u32 {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let max_len = left.len().max(right.len());
    if max_len == 0 {
        return 1_000;
    }
    let distance = damerau_levenshtein(&left, &right).min(max_len);
    ((max_len - distance) * 1_000 / max_len) as u32
}

fn damerau_levenshtein(left: &[char], right: &[char]) -> usize {
    let mut distances = vec![vec![0; right.len() + 1]; left.len() + 1];
    for (index, row) in distances.iter_mut().enumerate() {
        row[0] = index;
    }
    for (index, distance) in distances[0].iter_mut().enumerate() {
        *distance = index;
    }

    for left_index in 1..=left.len() {
        for right_index in 1..=right.len() {
            let substitution_cost = usize::from(left[left_index - 1] != right[right_index - 1]);
            distances[left_index][right_index] = (distances[left_index - 1][right_index] + 1)
                .min(distances[left_index][right_index - 1] + 1)
                .min(distances[left_index - 1][right_index - 1] + substitution_cost);
            if left_index > 1
                && right_index > 1
                && left[left_index - 1] == right[right_index - 2]
                && left[left_index - 2] == right[right_index - 1]
            {
                distances[left_index][right_index] = distances[left_index][right_index]
                    .min(distances[left_index - 2][right_index - 2] + 1);
            }
        }
    }
    distances[left.len()][right.len()]
}

fn search_media_card(meta: &MetaPreview) -> MediaCard {
    MediaCard {
        id: SharedString::from(&meta.id),
        title: SharedString::from(meta.title()),
        year: SharedString::from(meta.year_str().unwrap_or_default()),
        poster_path: SharedString::from(meta.poster.clone().unwrap_or_default()),
        poster: Image::default(),
        is_loaded: false,
        badge: SharedString::default(),
        watched: false,
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
                        supports_skip: c.supports_extra("skip"),
                        genre_options: catalog_genres(c),
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
    type MergedCatalog = (String, String, bool, Vec<String>, String);
    let mut by_type: HashMap<String, Vec<MergedCatalog>> = HashMap::new();
    for inst in &active {
        for cat in &inst.manifest.catalogs {
            by_type.entry(cat.type_.clone()).or_default().push((
                cat.id.clone(),
                cat.name.clone(),
                cat.supports_extra("skip"),
                catalog_genres(cat),
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
                .map(|(id, name, skip, genres, addon)| {
                    let label = format!("{addon} — {name}");
                    CatDef {
                        id: id.clone(),
                        supports_skip: *skip,
                        genre_options: genres.clone(),
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

pub(super) fn catalog_genres(catalog: &addons::Catalog) -> Vec<String> {
    let Some(extra) = catalog.extra.iter().find(|extra| extra.name == "genre") else {
        return Vec::new();
    };
    let choices = if extra.options.is_empty() {
        &catalog.genres
    } else {
        &extra.options
    };
    let mut seen = HashSet::new();
    choices
        .iter()
        .filter(|genre| !genre.is_empty() && seen.insert((*genre).clone()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extra(name: &str, options: &[&str]) -> addons::CatalogExtra {
        addons::CatalogExtra {
            name: name.into(),
            options: options.iter().map(|option| (*option).into()).collect(),
            is_required: false,
            options_required: false,
            extra: Default::default(),
        }
    }

    fn catalog(type_: &str, id: &str, extras: Vec<addons::CatalogExtra>) -> addons::Catalog {
        addons::Catalog {
            type_: type_.into(),
            id: id.into(),
            name: id.into(),
            extra: extras,
            genres: vec!["Drama".into(), "Comedy".into()],
            extra_fields: Default::default(),
        }
    }

    fn installed(enabled: bool, catalogs: Vec<addons::Catalog>) -> Installed {
        Installed {
            available: true,
            generation: 0,
            url: "https://catalog.example".into(),
            label: "Catalog addon".into(),
            enabled,
            configure_ok: None,
            manifest: Manifest {
                id: "catalog.example".into(),
                version: "1.0.0".into(),
                name: "Catalog addon".into(),
                description: None,
                resources: vec![],
                types: vec!["movie".into(), "series".into()],
                catalogs,
                id_prefixes: vec![],
                logo: None,
                background: None,
                extra: Default::default(),
            },
        }
    }

    #[test]
    fn global_search_targets_only_enabled_catalogs_with_search() {
        let addons = vec![
            installed(
                true,
                vec![
                    catalog(
                        "movie",
                        "searchable",
                        vec![extra("search", &[]), extra("skip", &[])],
                    ),
                    catalog("series", "browse-only", vec![extra("skip", &[])]),
                ],
            ),
            installed(
                false,
                vec![catalog("series", "disabled", vec![extra("search", &[])])],
            ),
        ];

        let targets = build_search_targets(&addons);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].type_, "movie");
        assert_eq!(targets[0].catalog_id, "searchable");
        assert!(targets[0].supports_skip);
    }

    #[test]
    fn genre_filter_uses_declared_genre_options_only() {
        let declared = catalog(
            "movie",
            "genres",
            vec![extra("genre", &["Action", "Comedy"]), extra("skip", &[])],
        );
        assert_eq!(catalog_genres(&declared), ["Action", "Comedy"]);

        let no_genre_extra = catalog("movie", "not-filterable", vec![extra("skip", &[])]);
        assert!(catalog_genres(&no_genre_extra).is_empty());
    }

    #[test]
    fn short_edits_keep_an_open_search_view_mounted() {
        assert!(search_results_open_after_edit("a", true));
        assert!(search_results_open_after_edit("", true));
        assert!(!search_results_open_after_edit("a", false));
        assert!(search_results_open_after_edit("ab", false));
    }

    #[test]
    fn search_relevance_prefers_exact_prefix_and_typo_matches() {
        let exact = search_match_score("STAR wars", "Star Wars", None);
        let prefix = search_match_score("star war", "Star Wars", None);
        let typo = search_match_score("satr wars", "Star Wars", None);
        let unrelated = search_match_score("satr wars", "Star Trek", None);

        assert!(exact > prefix);
        assert!(prefix > typo);
        assert!(typo > unrelated);
    }

    #[test]
    fn search_relevance_uses_year_but_does_not_fuzzy_match_numeric_tokens() {
        let exact_year = search_match_score("Movie 2020", "Movie", Some("2020"));
        let nearby_year = search_match_score("Movie 2021", "Movie", Some("2020"));
        assert!(exact_year > nearby_year);
    }

    #[test]
    fn search_history_is_recent_first_unique_and_bounded() {
        let mut history = Vec::new();
        remember_search_query(&mut history, "Star Wars");
        remember_search_query(&mut history, "Dune");
        remember_search_query(&mut history, "  star wars  ");
        assert_eq!(history, ["star wars", "Dune"]);
        remember_search_query(&mut history, "a");
        remember_search_query(&mut history, "");
        remember_search_query(&mut history, &"x".repeat(257));
        assert_eq!(history.len(), 2);
        for index in 0..30 {
            remember_search_query(&mut history, &format!("Movie {index}"));
        }
        assert_eq!(history.len(), 20);
        assert_eq!(history.first().unwrap(), "Movie 29");
        assert_eq!(history.last().unwrap(), "Movie 10");
    }

    #[test]
    fn catalog_display_prefix_is_optional_without_changing_identity() {
        let mut cat = CatDef {
            id: "popular".into(),
            label: "Addon A — Popular".into(),
            addon_label: "Addon A".into(),
            supports_skip: true,
            genre_options: vec![],
        };
        assert_eq!(catalog_display_label(&cat, false), "Addon A — Popular");
        assert_eq!(catalog_display_label(&cat, true), "Popular");
        assert_eq!(cat.label, "Addon A — Popular");
        let original_key = cat.label.clone();
        cat.addon_label = "Addon B".into();
        cat.label = "Addon B — Popular".into();
        assert_eq!(catalog_display_label(&cat, true), "Popular");
        assert_ne!(cat.label, original_key);
        cat.label = "Popular (popular)".into();
        assert_eq!(catalog_display_label(&cat, true), "Popular (popular)");
    }
}

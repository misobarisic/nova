//! Discover catalog: fetching, pagination, prefetching and pickers.
use super::*;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

fn reserve_metadata_prefetch(
    state: &mut Shared,
    type_: &str,
    id: &str,
    revision: u64,
    now: std::time::Instant,
) -> bool {
    let key = (type_.to_owned(), id.to_owned());
    if state.metadata_prefetch.get(&key).is_some_and(|previous| {
        previous.retry_at.is_none()
            || (previous.revision == revision && previous.retry_at.is_some_and(|at| now < at))
    }) {
        return false;
    }
    state.metadata_prefetch.insert(
        key,
        MetadataPrefetch {
            revision,
            retry_at: None,
        },
    );
    true
}

fn visible_search_poster_identities(
    previews: &[MetaPreview],
    fetch: &[(usize, String)],
    row_ids: &HashMap<usize, String>,
) -> HashMap<usize, (String, String)> {
    fetch
        .iter()
        .filter_map(|(index, _)| {
            let preview = previews.get(*index)?;
            (row_ids.get(index).is_some_and(|id| id == &preview.id))
                .then(|| (*index, (preview.type_.clone(), preview.id.clone())))
        })
        .collect()
}

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
            app.set_can_load_more(false);
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
        let mut search_row_ids: HashMap<usize, String> = HashMap::new();
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
                if searching {
                    search_row_ids.insert(i, card.id.to_string());
                }
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
        if searching && !fetch.is_empty() {
            // Snapshot only identities for cards that are about to be fetched.
            // Search previews can carry arbitrary addon JSON in `extra`, so a
            // full result-list clone on every scroll callback is needlessly
            // expensive. Matching row identities also drops stale candidates.
            let (generation, mut candidates) = {
                let state = self.shared.lock().unwrap();
                let candidates = visible_search_poster_identities(
                    &state.search_previews,
                    &fetch,
                    &search_row_ids,
                );
                (state.search_generation, candidates)
            };
            for (index, url) in fetch {
                if let Some((type_, id)) = candidates.remove(&index) {
                    self.fetch_search_card_poster(type_, id, url, generation);
                }
            }
        } else if !searching {
            for (i, url) in fetch {
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
        if exhausted || (!supports_skip && skip > 0) {
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
        self.fetch_catalog_page(skip > 0);
    }

    /// Core fetch: builds URLs (with `skip` extra when appending), fetches
    /// from all matching addons in parallel, and applies the merged result.
    pub(super) fn fetch_catalog_page(&self, append: bool) {
        let (urls, type_, catalog_id, genre, skip, generation) = {
            let state = self.shared.lock().unwrap();
            if state.chosen_type >= state.type_defs.len() {
                drop(state);
                self.cancel_catalog_page();
                return;
            }
            let ty = &state.type_defs[state.chosen_type];
            if state.chosen_catalog >= ty.catalogs.len() {
                drop(state);
                self.cancel_catalog_page();
                return;
            }
            let cat = &ty.catalogs[state.chosen_catalog];

            // Determine which addon URL(s) to fetch from.
            let urls: Vec<String> = if state.chosen_addon == usize::MAX {
                // "All addons" — find the addon that owns this catalog.
                state
                    .installed
                    .iter()
                    .filter(|a| a.enabled && a.url == cat.addon_url)
                    .map(|a| a.url.clone())
                    .collect()
            } else if state.chosen_addon < state.installed.len() {
                vec![state.installed[state.chosen_addon].url.clone()]
            } else {
                drop(state);
                self.cancel_catalog_page();
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

        self.shared.lock().unwrap().loading_more = true;
        if let Some(app) = self.app() {
            app.set_loading_more(true);
            app.set_can_load_more(false);
        }

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
            "nova: loading catalog (skip={skip}, append={append}) from {} addon(s)",
            fetch_urls.len()
        ));
        if fetch_urls.is_empty() {
            let bridge2 = bridge.clone();
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                bridge2.apply_catalog(generation, Vec::new(), append);
            });
            return;
        }

        let results = Arc::new(Mutex::new(Vec::<MetaPreview>::new()));
        let remaining = Arc::new(AtomicUsize::new(fetch_urls.len()));
        let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        for url in fetch_urls {
            let bridge = bridge.clone();
            let results = results.clone();
            let remaining = remaining.clone();
            let failed = failed.clone();
            net::fetch_bytes(url, move |result| {
                if let Ok(bytes) = result
                    && let Ok(metas) = Addon::parse_catalog(&bytes)
                {
                    results.lock().unwrap().extend(metas);
                } else {
                    failed.store(true, Ordering::SeqCst);
                }
                // Last continuation home merges onto the UI thread.
                if remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
                    let all = std::mem::take(&mut *results.lock().unwrap());
                    let bridge2 = bridge.clone();
                    let _ = crate::app::profiles::invoke_from_event_loop(move || {
                        if failed.load(Ordering::SeqCst) {
                            bridge2.catalog_page_failed(generation);
                        } else {
                            bridge2.apply_catalog(generation, all, append);
                        }
                    });
                }
            });
        }
    }

    fn cancel_catalog_page(&self) {
        // Invalid selections also invalidate any response from the old source.
        self.catalog_gen.fetch_add(1, Ordering::Relaxed);
        self.shared.lock().unwrap().loading_more = false;
        if let Some(app) = self.app() {
            app.set_loading_more(false);
            app.set_can_load_more(false);
        }
    }

    /// Failed batches leave the cursor intact so scrolling can retry the page.
    fn catalog_page_failed(&self, generation: u64) {
        if generation != self.catalog_gen.load(Ordering::Relaxed) {
            return;
        }
        self.shared.lock().unwrap().loading_more = false;
        if let Some(app) = self.app() {
            app.set_loading_more(false);
            app.set_can_load_more(true);
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
            let type_ = state
                .type_defs
                .get(state.chosen_type)
                .map(|t| t.type_.as_str())
                .unwrap_or_default();
            for meta in &mut new_metas {
                if meta.type_.is_empty() {
                    meta.type_ = type_.into();
                }
                reuse_discover_poster(
                    meta,
                    read_meta_header_for(&meta.type_, &meta.id).as_ref(),
                    &state.entries,
                );
            }
            if append {
                // Dedup new items against existing previews by id.
                new_metas.retain(|m| !state.previews.iter().any(|p| p.id == m.id));
                if new_metas.is_empty() {
                    state.catalog_exhausted = true;
                    state.loading_more = false;
                    drop(state);
                    if let Some(app) = self.app() {
                        app.set_loading_more(false);
                        app.set_can_load_more(false);
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
                            ..Default::default()
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
                            let bridge = self.clone();
                            let requested_url = url.clone();
                            net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
                                let _ = crate::app::profiles::invoke_from_event_loop(move || {
                                    let Some(pixels) = pixels else {
                                        bridge.recover_catalog_artwork(
                                            generation,
                                            offset + i,
                                            &requested_url,
                                        );
                                        return;
                                    };
                                    if generation != gen_counter.load(Ordering::Relaxed) {
                                        return;
                                    }
                                    if let Some(app) = weak.upgrade() {
                                        if app.get_catalog().row_data(offset + i).is_none_or(
                                            |row| row.poster_path.as_str() != requested_url,
                                        ) {
                                            return;
                                        }
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
                    let cached = decoded_cache_stats().0;
                    crate::web_log(&format!(
                        "nova: memory posters-cached={cached} cards={total}"
                    ));
                }
                for meta in &new_metas {
                    if meta
                        .poster
                        .as_deref()
                        .is_none_or(|url| url.trim().is_empty())
                    {
                        self.recover_missing_artwork(
                            &meta.type_,
                            &meta.id,
                            meta.poster.as_deref().unwrap_or_default(),
                        );
                    }
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
                        ..Default::default()
                    })
                    .collect();

                state.loading_more = false;
                state.next_skip = new_metas.len();
                state.catalog_exhausted = new_metas.is_empty();
                state.previews = new_metas.clone();
                state.catalog_gen = generation;
                drop(state);

                app.set_loading_more(false);
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
                    for (index, meta) in new_metas.iter().enumerate() {
                        if let Some(poster) = &meta.poster {
                            self.fetch_card_poster(false, index, poster.clone());
                        }
                    }
                }

                for meta in &new_metas {
                    if meta
                        .poster
                        .as_deref()
                        .is_none_or(|url| url.trim().is_empty())
                    {
                        self.recover_missing_artwork(
                            &meta.type_,
                            &meta.id,
                            meta.poster.as_deref().unwrap_or_default(),
                        );
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

    /// Prefetch metadata for a list of `(type_, id)` entries in the
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
                .filter(|a| a.enabled && a.available && a.manifest.has_meta())
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
        let revision = nova_providers::metadata_revision();
        let work: Vec<(String, String, Vec<String>)> = pairs
            .iter()
            .filter_map(|(type_, id)| {
                let urls: Vec<String> = installed
                    .iter()
                    .filter(|cand| cand.manifest.accepts("meta", type_, id))
                    .filter_map(|cand| Addon::new(&cand.url).ok())
                    .map(|a| a.meta_url(type_, id))
                    .collect();
                // Pending manifests and unsupported identities must not
                // consume the retry window before a fetch is possible.
                if urls.is_empty()
                    || !reserve_metadata_prefetch(
                        &mut self.shared.lock().unwrap(),
                        type_,
                        id,
                        revision,
                        std::time::Instant::now(),
                    )
                {
                    return None;
                }
                Some((type_.clone(), id.clone(), urls))
            })
            .collect();
        if work.is_empty() {
            return;
        }

        let bridge = self.clone();
        fn run(
            bridge: Bridge,
            mut work: VecDeque<(String, String, Vec<String>)>,
            mut cached: u32,
            revision: u64,
        ) {
            // Next pair?
            let Some((type_, id, mut urls)) = work.pop_front() else {
                let msg = text::prefetch_done(cached as usize);
                eprintln!("prefetch: {msg}");
                let _ = crate::app::profiles::invoke_from_event_loop(move || {
                    // Newly cached episode lists feed badges/checks/Home.
                    bridge.sync_reconcile_progress_aliases();
                    bridge.refresh_library_progress_ui();
                    if nova_providers::metadata_revision() != revision
                        && bridge.app().is_some_and(|app| app.get_show_library())
                    {
                        // A manifest may have changed while the old snapshot
                        // was fetching. Revisit those pairs with the new broker.
                        bridge.prefetch_library_meta();
                    }
                });
                return;
            };
            // Skip pairs whose modal was opened while prefetching.
            {
                let state = bridge.shared.lock().unwrap();
                if state
                    .modal_item
                    .as_ref()
                    .map(|m| m.id == id && m.type_ == type_)
                    .unwrap_or(false)
                {
                    drop(state);
                    bridge.finish_metadata_prefetch(&type_, &id, false);
                    run(bridge, work, cached, revision);
                    return;
                }
            }
            if urls.is_empty() {
                eprintln!("prefetch: no addon provided episodes for {type_}/{id}");
                bridge.finish_metadata_prefetch(&type_, &id, false);
                run(bridge, work, cached, revision);
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
                        Ok(Some(item)) if meta_matches_request(&item, &type_, &id) => {
                            // Cache the header (background/description/genres/
                            // year) alongside episodes: the prefetch used to
                            // discard it, so pills + synopsis still needed a
                            // network round-trip on open.
                            if let Some(poster) = item
                                .preview
                                .poster
                                .as_ref()
                                .filter(|url| !url.trim().is_empty())
                            {
                                let bridge = bridge2.clone();
                                let poster = poster.clone();
                                let poster_type = type_.clone();
                                let poster_id = id.clone();
                                let _ = crate::app::profiles::invoke_from_event_loop(move || {
                                    bridge.load_discover_poster(poster_type, poster_id, poster);
                                });
                            }
                            if bridge2.cache_background_meta(&type_, &id, &item) > 0 {
                                cached += 1;
                                pair_done = true;
                            }
                            // Movies need the same identity/artwork enrichment
                            // even though they have no season episode list.
                            pair_done |= type_ == "movie";
                        }
                        Ok(_) => {
                            eprintln!("prefetch: addon returned None for {type_}/{id}");
                        }
                        Err(e) => {
                            eprintln!("prefetch: meta parse failed for {type_}/{id}: {e}");
                        }
                    },
                }
                if pair_done {
                    bridge2.finish_metadata_prefetch(&type_, &id, true);
                    run(bridge2, work, cached, revision);
                } else {
                    // Try the next addon URL for this pair.
                    work.push_front((type_, id, urls));
                    run(bridge2, work, cached, revision);
                }
            });
        }

        run(bridge, work.into(), 0, revision);
    }

    /// Both missing-metadata prefetch and failed-image repair receive enriched
    /// native responses. Keep their mappings instead of discarding everything
    /// except the poster, so Library recovery does not require opening Detail.
    pub(super) fn cache_background_meta(&self, type_: &str, id: &str, item: &MetaItem) -> usize {
        if !meta_matches_request(item, type_, id) {
            return 0;
        }
        let header = meta_header_from_item(item);
        merge_meta_header_for(type_, id, &header);
        if !header.background_url.is_empty() {
            self.persist_backdrop_for(id, &header.background_url);
        }
        self.persist_header_for(id, &header.genres, &header.description, &header.year, false);
        if item.videos.is_empty() {
            return 0;
        }
        write_episode_meta_cache_for(type_, id, item).len()
    }

    fn finish_metadata_prefetch(&self, type_: &str, id: &str, success: bool) {
        if let Some(prefetch) = self
            .shared
            .lock()
            .unwrap()
            .metadata_prefetch
            .get_mut(&(type_.into(), id.into()))
        {
            prefetch.retry_at = Some(
                std::time::Instant::now() + Duration::from_secs(if success { 5 * 60 } else { 60 }),
            );
        }
    }

    /// Fill missing Library metadata through Detail's enrichment transport.
    /// Complete cached entries keep their cache; failed/missing images trigger
    /// their own repair path, which also saves any newly confirmed mappings.
    pub(super) fn prefetch_library_meta(&self) {
        let mut entries = {
            let state = self.shared.lock().unwrap();
            state.entries.clone()
        };
        if let Some(app) = self.app() {
            let order: HashMap<_, _> = app
                .get_library()
                .iter()
                .enumerate()
                .map(|(index, row)| (row.id.to_string(), index))
                .collect();
            // Follow the displayed filter/sort order rather than old insertion
            // order, so the cards the user is viewing get their mappings first.
            entries.sort_by_key(|entry| order.get(&entry.id).copied().unwrap_or(usize::MAX));
        }
        let pairs: Vec<(String, String)> = entries
            .iter()
            .filter_map(|e| {
                if e.id.is_empty() || e.type_.is_empty() {
                    return None;
                }
                let has_text = !e.description.is_empty()
                    || !e.genres.is_empty()
                    || header_text_cached(&e.type_, &e.id);
                let has_episodes = e.type_ == "movie"
                    || read_episodes_cache_for(&e.type_, &e.id)
                        .is_some_and(|videos| !videos.is_empty());
                if has_text && has_episodes {
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
            discover_addon_indices(&state.installed)
                .into_iter()
                .find(|&index| state.installed[index].label == label)
                .unwrap_or(usize::MAX)
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
        if !changed {
            return;
        }
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
            let catalog_count = state
                .type_defs
                .get(state.chosen_type)
                .map(|t| t.catalogs.len())
                .unwrap_or(0);
            let idx = state
                .type_defs
                .get(state.chosen_type)
                .and_then(|t| t.catalogs.iter().position(|c| c.label == label))
                .unwrap_or(0);
            (idx != state.chosen_catalog || catalog_count == 0, idx)
        };
        if !changed {
            return;
        }
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

    pub(super) fn activate_search_filters(&self) {
        let Some(app) = self.app() else { return };
        if !app.get_discover_search_filters_open() {
            self.apply_search_filters_to_ui();
            app.set_discover_search_filters_open(true);
        }
    }

    /// Picker indices belong to the temporary search models, never to the
    /// browse selectors. Catalogs retain their addon/type/ID identity even
    /// when display prefixes are hidden or two sources have the same name.
    pub(super) fn pick_search_filter(&self, kind: i32, index: i32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        self.activate_search_filters();
        let changed = {
            let mut state = self.shared.lock().unwrap();
            let previous = state.search_filters.clone();
            let options = current_search_filter_options(&mut state);
            let filters = &mut state.search_filters;
            match kind {
                0 if index <= options.addons.len() => {
                    let value = index
                        .checked_sub(1)
                        .map_or_else(String::new, |i| options.addons[i].0.clone());
                    if filters.addon_url != value {
                        filters.addon_url = value;
                        filters.catalog = None;
                        filters.genre.clear();
                    }
                }
                1 if index <= options.types.len() => {
                    let value = index
                        .checked_sub(1)
                        .map_or_else(String::new, |i| options.types[i].clone());
                    if filters.type_ != value {
                        filters.type_ = value;
                        filters.catalog = None;
                        filters.genre.clear();
                    }
                }
                2 if index <= options.catalogs.len() => {
                    let value = index
                        .checked_sub(1)
                        .map(|i| options.catalogs[i].source.clone());
                    if filters.catalog != value {
                        filters.catalog = value;
                        filters.genre.clear();
                    }
                }
                3 if index <= options.genres.len() => {
                    filters.genre = index
                        .checked_sub(1)
                        .map_or_else(String::new, |i| options.genres[i].clone());
                }
                _ => return,
            }
            *filters != previous
        };
        self.apply_search_filters_to_ui();
        if changed && let Some(app) = self.app() {
            self.start_search(&app.get_search_text(), false);
        }
    }

    fn apply_search_filters_to_ui(&self) {
        let Some(app) = self.app() else { return };
        let (options, indices) = {
            let mut state = self.shared.lock().unwrap();
            let options = current_search_filter_options(&mut state);
            let filters = &state.search_filters;
            let indices = [
                options
                    .addons
                    .iter()
                    .position(|(url, _)| url == &filters.addon_url),
                options
                    .types
                    .iter()
                    .position(|type_| type_ == &filters.type_),
                options
                    .catalogs
                    .iter()
                    .position(|catalog| Some(&catalog.source) == filters.catalog.as_ref()),
                options
                    .genres
                    .iter()
                    .position(|genre| genre == &filters.genre),
            ]
            .map(|index| index.map_or(0, |index| (index + 1) as i32));
            (options, indices)
        };
        app.set_search_addon_names(search_filter_model(
            "All addons",
            options.addons.into_iter().map(|(_, label)| label),
        ));
        app.set_search_type_names(search_filter_model("All types", options.types));
        app.set_search_catalog_names(search_filter_model(
            "All catalogs",
            options.catalogs.iter().map(|catalog| catalog.name.clone()),
        ));
        app.set_search_catalog_labels(search_filter_model(
            "All catalogs",
            options.catalogs.into_iter().map(|catalog| catalog.label),
        ));
        app.set_search_genre_names(search_filter_model("All genres", options.genres));
        app.set_search_addon_combo_idx(indices[0]);
        app.set_search_type_combo_idx(indices[1]);
        app.set_search_catalog_combo_idx(indices[2]);
        app.set_search_genre_combo_idx(indices[3]);
    }

    /// Addon changes can remove a selected source or introduce new search
    /// catalogs. Rebuild this session's choices and reject old replies before
    /// querying the revised set; unrelated browse changes keep search intact.
    pub(super) fn refresh_search_filters(&self) {
        self.apply_search_filters_to_ui();
        let Some(app) = self.app() else { return };
        if !app.get_discover_search_filters_open() {
            return;
        }
        let changed = {
            let state = self.shared.lock().unwrap();
            let targets = filtered_search_targets(&state.installed, &state.search_filters);
            targets.len() != state.search_targets.len()
                || !targets
                    .iter()
                    .zip(&state.search_targets)
                    .all(|(next, old)| {
                        next.source() == old.source()
                            && next.genre == old.genre
                            && next.supports_skip == old.supports_skip
                    })
        };
        if changed && app.get_search_text().trim().chars().count() >= 2 {
            self.start_search(&app.get_search_text(), false);
        }
    }

    /// Invalidate the previous query as soon as the user edits the field.
    /// Schedule the next request after a short quiet period; a late response
    /// from the old query must not repopulate results while the user types.
    pub(super) fn search_edited(&self, text: &str) {
        self.activate_search_filters();
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
            state.search_pending_requests = 0;
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
            let query = text.to_string();
            let bridge = self.clone();
            let context = profiles::capture();
            slint::Timer::single_shot(Duration::from_millis(350), move || {
                if !profiles::is_current(&context) {
                    return;
                }
                let current = bridge.shared.lock().unwrap().search_generation == generation;
                if current {
                    bridge.start_search(&query, false);
                }
            });
        }
    }

    pub(super) fn submit_search(&self, text: &str) {
        self.start_search(text, true);
    }

    fn start_search(&self, text: &str, focus_search: bool) {
        self.activate_search_filters();
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
            state.search_targets = filtered_search_targets(&state.installed, &state.search_filters);
            state.search_pending_requests = 0;
            state.search_loading_more = false;
            state.search_poster_inflight.clear();
            (generation, state.search_targets.len())
        };

        if let Some(app) = self.app() {
            // Debounced requests must not rewrite an editor whose trailing
            // space is waiting for the next word (especially on Windows).
            if focus_search {
                app.set_search_text(SharedString::from(text));
            }
            app.set_discover_search_animate_results(true);
            app.set_search_results(Rc::new(VecModel::<MediaCard>::from(vec![])).into());
            app.set_discover_search_open(true);
            app.set_discover_search_scroll_y(0.0);
            app.set_discover_kb_idx(0);
            if focus_search {
                app.set_discover_kb_zone(2);
                app.set_discover_kb_ctl(3);
            }
            app.set_search_loading(true);
            app.set_search_loading_more(false);
            app.set_search_can_load_more(false);
            app.set_search_empty_hint(SharedString::from(text::tr("No results for this search.")));
        }

        let targets = (0..target_count).collect();
        self.fetch_search_pages(generation, trimmed, targets, false);
    }

    /// Search the session's enabled catalogs whose manifests declare `search`.
    /// Browse selection is intentionally not modified by a query.
    fn fetch_search_pages(
        &self,
        generation: u64,
        query: String,
        target_indices: Vec<usize>,
        append: bool,
    ) {
        let work: Vec<(usize, String, String)> = {
            let mut state = self.shared.lock().unwrap();
            if generation != state.search_generation {
                return;
            }
            let work = target_indices
                .into_iter()
                .filter_map(|index| {
                    let target = state.search_targets.get(index)?;
                    let addon = Addon::new(&target.addon_url).ok()?;
                    let mut extras = vec![("search", query.clone())];
                    if !target.genre.is_empty() {
                        extras.push(("genre", target.genre.clone()));
                    }
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
                .collect::<Vec<_>>();
            state.search_pending_requests = work.len();
            work
        };

        if work.is_empty() {
            let bridge = self.clone();
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                bridge.apply_search_results(generation, Vec::new(), append);
            });
            return;
        }

        for (target_index, url, type_) in work {
            let bridge = self.clone();
            net::fetch_bytes(url, move |result| {
                let mut page = result
                    .ok()
                    .and_then(|bytes| Addon::parse_catalog(&bytes).ok());
                if let Some(metas) = &mut page {
                    for meta in metas {
                        if meta.type_.is_empty() {
                            meta.type_ = type_.clone();
                        }
                    }
                }
                let _ = crate::app::profiles::invoke_from_event_loop(move || {
                    bridge.apply_search_results(generation, vec![(target_index, page)], append);
                });
            });
        }
    }

    /// Apply each completed source page immediately, preserving deterministic
    /// source ordering and independent cursors for catalogs that support skip.
    fn apply_search_results(
        &self,
        generation: u64,
        pages: Vec<(usize, Option<Vec<MetaPreview>>)>,
        append: bool,
    ) {
        let Some(app) = self.app() else { return };
        let (old_previews, ranked_metas, new_count, can_load_more, pending) = {
            let mut state = self.shared.lock().unwrap();
            if generation != state.search_generation {
                return;
            }
            let old_previews = state.search_previews.clone();

            for (index, metas) in pages {
                record_search_page_outcome(&mut state.search_targets, index, metas);
            }

            let query = state.search.clone();
            let merged = ranked_search_results(&query, &state.search_targets);
            let old_keys: HashSet<(String, String)> = old_previews
                .iter()
                .map(|meta| (meta.type_.clone(), meta.id.clone()))
                .collect();
            let new_count = merged
                .iter()
                .filter(|meta| !old_keys.contains(&(meta.type_.clone(), meta.id.clone())))
                .count();
            state.search_previews = merged;
            let ranked_metas = state.search_previews.clone();
            state.search_pending_requests = state.search_pending_requests.saturating_sub(1);
            let pending = state.search_pending_requests;
            if pending == 0 {
                state.search_loading_more = false;
            }
            let can_load_more =
                pending == 0 && state.search_targets.iter().any(|target| !target.exhausted);
            (
                old_previews,
                ranked_metas,
                new_count,
                can_load_more,
                pending,
            )
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
            let mut known = meta.clone();
            reuse_discover_poster(
                &mut known,
                read_meta_header_for(&meta.type_, &meta.id).as_ref(),
                &self.shared.lock().unwrap().entries,
            );
            let mut card = search_media_card(&known);
            if let Some(old) = old_cards.remove(&key)
                && old.poster_path == card.poster_path
            {
                card.poster = old.poster;
                card.is_loaded = old.is_loaded;
            }
            if !card.is_loaded {
                poster_jobs.push((
                    meta.type_.clone(),
                    meta.id.clone(),
                    card.poster_path.to_string(),
                ));
            }
            cards.push(card);
        }
        update_search_result_model(&app, &old_previews, &ranked_metas, cards);

        for (type_, id, poster) in poster_jobs {
            self.fetch_search_card_poster(type_, id, poster, generation);
        }
        app.set_search_loading(pending > 0 && !append);
        app.set_search_loading_more(pending > 0 && append);
        app.set_search_can_load_more(can_load_more);
        if !append && pending == 0 {
            let query = self.shared.lock().unwrap().search.clone();
            self.remember_search(&query);
        }
        crate::web_log(&format!(
            "nova: search source applied ({} new items, pending={pending}, append={append}, can_load_more={can_load_more})",
            new_count,
        ));
    }

    pub(super) fn load_more_search(&self) {
        let (generation, query, targets) = {
            let mut state = self.shared.lock().unwrap();
            if state.search_loading_more || state.search_pending_requests > 0 {
                return;
            }
            let targets: Vec<usize> = state
                .search_targets
                .iter()
                .enumerate()
                .filter_map(|(index, target)| (!target.exhausted).then_some(index))
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
            state.search_filters = SearchFilters::default();
            state.search_targets.clear();
            state.search_poster_inflight.clear();
            state.search_pending_requests = 0;
            state.search_loading_more = false;
        }
        if let Some(app) = self.app() {
            app.set_search_text(SharedString::default());
            app.set_search_loading(false);
            app.set_search_loading_more(false);
            app.set_search_can_load_more(false);
            app.set_discover_search_open(false);
            app.set_discover_search_focused(false);
            app.set_discover_search_filters_open(false);
        }
        self.apply_search_filters_to_ui();
    }

    // Search history is deliberately local: it never enters the sync domains.
    pub(super) fn load_search_history(&self) {
        self.apply_search_history(&read_search_history());
    }

    fn apply_search_history(&self, history: &[SearchHistoryEntry]) {
        if let Some(app) = self.app() {
            app.set_discover_search_history(
                Rc::new(VecModel::from(
                    history
                        .iter()
                        .map(|entry| SharedString::from(entry.query.as_str()))
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
            app.set_discover_search_history_contexts(
                Rc::new(VecModel::from(
                    history
                        .iter()
                        .map(|entry| SharedString::from(entry.caption.as_str()))
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
        }
    }

    fn remember_search(&self, query: &str) {
        let entry = {
            let state = self.shared.lock().unwrap();
            SearchHistoryEntry::new(query, &state.search_filters, &state.installed)
        };
        let mut history = read_search_history();
        remember_search_entry(&mut history, entry);
        write_json("discover:search_history:v2", &history);
        self.apply_search_history(&history);
    }

    pub(super) fn replay_search_history(&self, index: i32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let Some(entry) = read_search_history().get(index).cloned() else {
            return;
        };
        {
            let mut state = self.shared.lock().unwrap();
            state.search_filters = restore_search_context(&entry.context, &state.installed);
        }
        self.apply_search_filters_to_ui();
        if let Some(app) = self.app() {
            app.set_search_text(entry.query.clone().into());
        }
        self.submit_search(&entry.query);
    }

    pub(super) fn clear_search_history(&self) {
        let history = Vec::<SearchHistoryEntry>::new();
        // An explicit empty v2 list prevents legacy entries reappearing.
        write_json("discover:search_history:v2", &history);
        write_json("discover:search_history", &Vec::<String>::new());
        self.apply_search_history(&history);
    }

    pub(super) fn remove_search_history_item_at(&self, index: i32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let mut history = read_search_history();
        if index >= history.len() {
            return;
        }
        history.remove(index);
        write_json("discover:search_history:v2", &history);
        self.apply_search_history(&history);
    }

    pub(super) fn remove_search_history_item(&self, query: &str) {
        let mut history = read_search_history();
        history.retain(|entry| entry.query != query);
        write_json("discover:search_history:v2", &history);
        self.apply_search_history(&history);
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
            let supports_search = !build_search_targets(&state.installed).is_empty();
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
        self.apply_search_filters_to_ui();
    }

    /// Refresh global-search availability without rebuilding picker models —
    /// used after addon installs/toggles.
    pub(super) fn apply_search_support_to_ui(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let (hint, searchable) = {
            let state = self.shared.lock().unwrap();
            let searchable = !build_search_targets(&state.installed).is_empty();
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
        // Search-only catalogs stay usable even when the browse picker is empty.
        self.apply_search_support_to_ui();

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

struct SearchCatalogOption {
    source: SearchCatalogSource,
    name: String,
    label: String,
}

struct SearchFilterOptions {
    addons: Vec<(String, String)>,
    types: Vec<String>,
    catalogs: Vec<SearchCatalogOption>,
    genres: Vec<String>,
}

fn current_search_filter_options(state: &mut Shared) -> SearchFilterOptions {
    search_filter_options(
        &state.installed,
        &mut state.search_filters,
        state.cache_settings.discover_catalog_addon_names,
    )
}

fn search_filter_options(
    installed: &[Installed],
    filters: &mut SearchFilters,
    show_addon_names: bool,
) -> SearchFilterOptions {
    let targets = build_search_targets(installed);
    let addons: Vec<(String, String)> = installed
        .iter()
        .filter(|addon| targets.iter().any(|target| target.addon_url == addon.url))
        .map(|addon| (addon.url.clone(), addon.label.clone()))
        .collect();
    if !filters.addon_url.is_empty() && !addons.iter().any(|(url, _)| url == &filters.addon_url) {
        *filters = SearchFilters::default();
    }

    let mut types = Vec::new();
    for target in &targets {
        if (filters.addon_url.is_empty() || target.addon_url == filters.addon_url)
            && !types.contains(&target.type_)
        {
            types.push(target.type_.clone());
        }
    }
    if !filters.type_.is_empty() && !types.contains(&filters.type_) {
        filters.type_.clear();
        filters.catalog = None;
    }

    let scoped: Vec<&SearchTarget> = targets
        .iter()
        .filter(|target| {
            (filters.addon_url.is_empty() || target.addon_url == filters.addon_url)
                && (filters.type_.is_empty() || target.type_ == filters.type_)
        })
        .collect();
    let catalogs: Vec<SearchCatalogOption> = scoped
        .iter()
        .filter_map(|target| {
            let addon = installed
                .iter()
                .find(|addon| addon.url == target.addon_url)?;
            let catalog = addon.manifest.catalogs.iter().find(|catalog| {
                catalog.supports_extra("search")
                    && catalog.type_ == target.type_
                    && catalog.id == target.catalog_id
            })?;
            let mut label = if catalog.name.is_empty() {
                catalog.id.clone()
            } else {
                catalog.name.clone()
            };
            if addon
                .manifest
                .catalogs
                .iter()
                .filter(|other| {
                    other.type_ == catalog.type_
                        && other.name == catalog.name
                        && other.supports_extra("search")
                })
                .count()
                > 1
            {
                label = format!("{label} ({})", catalog.id);
            }
            if filters.type_.is_empty() {
                label = format!("{label} ({})", catalog.type_);
            }
            let name = if filters.addon_url.is_empty() {
                format!("{} — {label}", addon.label)
            } else {
                label.clone()
            };
            if show_addon_names {
                label = name.clone();
            }
            Some(SearchCatalogOption {
                source: target.source(),
                name,
                label,
            })
        })
        .collect();
    if filters
        .catalog
        .as_ref()
        .is_some_and(|source| !catalogs.iter().any(|catalog| &catalog.source == source))
    {
        filters.catalog = None;
    }

    let mut genres = Vec::new();
    for target in scoped {
        if filters
            .catalog
            .as_ref()
            .is_none_or(|source| *source == target.source())
        {
            for genre in &target.genre_options {
                if !genres.contains(genre) {
                    genres.push(genre.clone());
                }
            }
        }
    }
    if !filters.genre.is_empty() && !genres.contains(&filters.genre) {
        filters.genre.clear();
    }
    SearchFilterOptions {
        addons,
        types,
        catalogs,
        genres,
    }
}

fn search_filter_model(
    all_label: &'static str,
    choices: impl IntoIterator<Item = String>,
) -> slint::ModelRc<SharedString> {
    Rc::new(VecModel::from(
        std::iter::once(SharedString::from(text::tr(all_label)))
            .chain(choices.into_iter().map(SharedString::from))
            .collect::<Vec<_>>(),
    ))
    .into()
}

fn filtered_search_targets(installed: &[Installed], filters: &SearchFilters) -> Vec<SearchTarget> {
    build_search_targets(installed)
        .into_iter()
        .filter_map(|mut target| {
            if (!filters.addon_url.is_empty() && filters.addon_url != target.addon_url)
                || (!filters.type_.is_empty() && filters.type_ != target.type_)
                || filters
                    .catalog
                    .as_ref()
                    .is_some_and(|source| *source != target.source())
                || (!filters.genre.is_empty() && !target.genre_options.contains(&filters.genre))
            {
                return None;
            }
            target.genre = filters.genre.clone();
            Some(target)
        })
        .collect()
}

fn build_search_targets(installed: &[Installed]) -> Vec<SearchTarget> {
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for addon in installed
        .iter()
        .filter(|addon| addon.enabled && addon.available)
    {
        if Addon::new(&addon.url).is_err() {
            continue;
        }
        for catalog in &addon.manifest.catalogs {
            if !addon
                .manifest
                .search_catalogs(&catalog.type_)
                .any(|c| c.id == catalog.id)
                || !seen.insert((addon.url.clone(), catalog.type_.clone(), catalog.id.clone()))
            {
                continue;
            }
            targets.push(SearchTarget {
                addon_url: addon.url.clone(),
                type_: catalog.type_.clone(),
                catalog_id: catalog.id.clone(),
                genre_options: catalog_genres(catalog),
                genre: String::new(),
                supports_skip: catalog.supports_extra("skip"),
                next_skip: 0,
                exhausted: false,
                results: Vec::new(),
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

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct SearchHistoryContext {
    addon_url: String,
    type_: String,
    catalog: Option<(String, String, String)>,
    genre: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct SearchHistoryEntry {
    query: String,
    #[serde(default)]
    context: SearchHistoryContext,
    #[serde(default)]
    caption: String,
}

impl SearchHistoryEntry {
    fn new(query: &str, filters: &SearchFilters, installed: &[Installed]) -> Self {
        let context = SearchHistoryContext {
            addon_url: filters.addon_url.clone(),
            type_: filters.type_.clone(),
            catalog: filters.catalog.as_ref().map(|source| {
                (
                    source.addon_url.clone(),
                    source.type_.clone(),
                    source.catalog_id.clone(),
                )
            }),
            genre: filters.genre.clone(),
        };
        let owner = filters
            .catalog
            .as_ref()
            .map_or(filters.addon_url.as_str(), |source| {
                source.addon_url.as_str()
            });
        let mut labels = vec![
            installed
                .iter()
                .find(|addon| addon.url == owner)
                .map_or_else(
                    || text::tr("All addons").to_string(),
                    |addon| addon.label.clone(),
                ),
        ];
        if !filters.type_.is_empty() {
            labels.push(filters.type_.clone());
        }
        if let Some(source) = &filters.catalog {
            let name = installed
                .iter()
                .find(|addon| addon.url == source.addon_url)
                .and_then(|addon| {
                    addon.manifest.catalogs.iter().find(|catalog| {
                        catalog.type_ == source.type_ && catalog.id == source.catalog_id
                    })
                })
                .map(|catalog| {
                    if catalog.name.is_empty() {
                        &catalog.id
                    } else {
                        &catalog.name
                    }
                });
            labels.push(name.unwrap_or(&source.catalog_id).clone());
        }
        if !filters.genre.is_empty() {
            labels.push(filters.genre.clone());
        }
        Self {
            query: query.trim().to_string(),
            context,
            caption: labels.join(" · "),
        }
    }
}

fn restore_search_context(
    context: &SearchHistoryContext,
    installed: &[Installed],
) -> SearchFilters {
    let mut filters = SearchFilters {
        addon_url: context.addon_url.clone(),
        type_: context.type_.clone(),
        genre: context.genre.clone(),
        catalog: context
            .catalog
            .as_ref()
            .map(|(addon_url, type_, catalog_id)| SearchCatalogSource {
                addon_url: addon_url.clone(),
                type_: type_.clone(),
                catalog_id: catalog_id.clone(),
            }),
    };
    if let Some(source) = &filters.catalog {
        let targets = build_search_targets(installed);
        if !targets.iter().any(|target| target.source() == *source) {
            // A catalog selected under All addons still owns a precise addon.
            // Keep that owner when its catalog disappears instead of widening
            // immediately to unrelated providers.
            filters.addon_url = source.addon_url.clone();
            if filters.type_.is_empty() {
                filters.type_ = source.type_.clone();
            }
            filters.catalog = None;
        }
    }
    search_filter_options(installed, &mut filters, false);
    filters
}

fn read_search_history() -> Vec<SearchHistoryEntry> {
    normalize_search_history(
        read_json::<Vec<SearchHistoryEntry>>("discover:search_history:v2"),
        || read_json::<Vec<String>>("discover:search_history").unwrap_or_default(),
    )
}

fn normalize_search_history(
    current: Option<Vec<SearchHistoryEntry>>,
    legacy: impl FnOnce() -> Vec<String>,
) -> Vec<SearchHistoryEntry> {
    let stored = current.unwrap_or_else(|| {
        legacy()
            .into_iter()
            .map(|query| SearchHistoryEntry {
                query,
                context: SearchHistoryContext::default(),
                caption: text::tr("All addons").to_string(),
            })
            .collect()
    });
    let mut history = Vec::new();
    for entry in stored.into_iter().rev() {
        remember_search_entry(&mut history, entry);
    }
    history
}

fn remember_search_entry(history: &mut Vec<SearchHistoryEntry>, mut entry: SearchHistoryEntry) {
    entry.query = entry.query.trim().to_string();
    if !(2..=256).contains(&entry.query.chars().count()) {
        return;
    }
    let key = entry.query.to_lowercase();
    history.retain(|old| old.query.to_lowercase() != key || old.context != entry.context);
    history.insert(0, entry);
    history.truncate(20);
}

/// Flatten all delivered source pages in declared source order. Stable
/// deduplication means an item keeps the metadata from its highest-priority
/// source even when that source responds later.
fn unique_search_results(targets: &[SearchTarget]) -> Vec<MetaPreview> {
    let mut seen = HashSet::new();
    targets
        .iter()
        .flat_map(|target| &target.results)
        .filter(|meta| !meta.id.is_empty() && seen.insert((meta.type_.clone(), meta.id.clone())))
        .cloned()
        .collect()
}

fn update_search_result_model(
    app: &AppWindow,
    old_previews: &[MetaPreview],
    new_previews: &[MetaPreview],
    cards: Vec<MediaCard>,
) {
    let model = app.get_search_results();
    let Some(model) = model.as_any().downcast_ref::<VecModel<MediaCard>>() else {
        app.set_search_results(Rc::new(VecModel::from(cards)).into());
        return;
    };
    if model.row_count() != old_previews.len() {
        app.set_search_results(Rc::new(VecModel::from(cards)).into());
        return;
    }

    let mut current_keys: Vec<(String, String)> = old_previews
        .iter()
        .map(|meta| (meta.type_.clone(), meta.id.clone()))
        .collect();
    for (index, (meta, card)) in new_previews.iter().zip(cards).enumerate() {
        let key = (meta.type_.clone(), meta.id.clone());
        if current_keys.get(index) != Some(&key) {
            if let Some(previous_index) = current_keys
                .iter()
                .enumerate()
                .skip(index + 1)
                .find_map(|(at, old_key)| (old_key == &key).then_some(at))
            {
                let previous_card = model.remove(previous_index);
                model.insert(index, previous_card);
                current_keys.remove(previous_index);
                current_keys.insert(index, key);
            } else {
                model.insert(index, card.clone());
                current_keys.insert(index, key);
            }
        }
        model.set_row_data(index, card);
    }
    while current_keys.len() > new_previews.len() {
        model.remove(current_keys.len() - 1);
        current_keys.pop();
    }
}

fn record_search_page_outcome(
    targets: &mut [SearchTarget],
    index: usize,
    page: Option<Vec<MetaPreview>>,
) {
    if let Some(metas) = page {
        record_search_source_page(targets, index, metas);
    }
}

fn record_search_source_page(targets: &mut [SearchTarget], index: usize, metas: Vec<MetaPreview>) {
    let Some(target) = targets.get_mut(index) else {
        return;
    };
    if metas.is_empty() || !target.supports_skip {
        target.exhausted = true;
    } else {
        target.next_skip += metas.len();
    }
    target.results.extend(metas);
}

fn ranked_search_results(query: &str, targets: &[SearchTarget]) -> Vec<MetaPreview> {
    let mut results = unique_search_results(targets);
    // Stable sorting preserves declared source and provider result order for
    // equally relevant rows.
    results.sort_by_cached_key(|meta| std::cmp::Reverse(search_relevance_score(query, meta)));
    results
}

fn search_relevance_score(query: &str, meta: &MetaPreview) -> u32 {
    let title = meta.title();
    let year = meta.year_str();
    search_match_score(query, &title, year.as_deref())
}

/// Rank exact, prefix, phrase, complete-token and typo-tolerant title matches
/// in descending tiers. Weak partial-token matches remain at the bottom, and
/// all provider results stay visible regardless of score.
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

    // Match exact tokens first and then the strongest remaining fuzzy pairs.
    // This is a one-to-one assignment, so one generic title word cannot stand
    // in for several query words. Short words stay exact-only to avoid noisy
    // fuzzy hits such as matching unrelated three-letter titles.
    let mut candidates = Vec::new();
    for (query_index, query_token) in query_tokens.iter().enumerate() {
        for (title_index, title_token) in title_tokens.iter().enumerate() {
            let similarity = if query_token == title_token {
                1_000
            } else if query_token.chars().all(char::is_numeric)
                || query_token.chars().count() < 4
                || title_token.chars().count() < 4
            {
                0
            } else {
                normalized_edit_similarity(query_token, title_token)
            };
            if similarity == 1_000 || similarity >= 700 {
                candidates.push((similarity, query_index, title_index));
            }
        }
    }
    candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
    });

    let mut used_query = vec![false; query_tokens.len()];
    let mut used_title = vec![false; title_tokens.len()];
    let mut matches = Vec::new();
    for (similarity, query_index, title_index) in candidates {
        if !used_query[query_index] && !used_title[title_index] {
            used_query[query_index] = true;
            used_title[title_index] = true;
            matches.push((query_index, title_index, similarity));
        }
    }

    let exact_hits = matches
        .iter()
        .filter(|(_, _, score)| *score == 1_000)
        .count();
    let fuzzy_hits = matches.len() - exact_hits;
    if matches.len() == query_tokens.len() {
        if fuzzy_hits == 0 {
            return 720_000;
        }
        let average_fuzzy = matches
            .iter()
            .filter(|(_, _, score)| *score < 1_000)
            .map(|(_, _, score)| *score)
            .sum::<u32>()
            / fuzzy_hits as u32;
        return 600_000 + exact_hits.min(20) as u32 * 1_000 + average_fuzzy / 20;
    }

    // Partial matching needs at least two useful tokens and roughly two
    // thirds of the query. This keeps a shared word from ranking above a
    // complete typo-tolerant title match.
    if matches.len() > 1 && matches.len() * 3 >= query_tokens.len() * 2 {
        return 300_000
            + (matches.len() as u32 * 100_000 / query_tokens.len() as u32)
            + exact_hits.min(20) as u32 * 1_000;
    }

    // Compare the whole query with short title-word windows for joined or
    // split names (for example "spiderman" / "spider man"). Keep numeric
    // tokens exact even in this fallback.
    let numeric_tokens_match = query_tokens
        .iter()
        .filter(|token| token.chars().all(char::is_numeric))
        .all(|query_token| title_tokens.contains(query_token));
    if query.chars().count() < 3 || !numeric_tokens_match {
        return 0;
    }
    let min_window = query_tokens.len().saturating_sub(1).max(1);
    let max_window = (query_tokens.len() + 1).min(title_tokens.len());
    if min_window > max_window {
        return 0;
    }
    let mut best_similarity = 0;
    for width in min_window..=max_window {
        for start in 0..=title_tokens.len() - width {
            let window = title_tokens[start..start + width].join(" ");
            best_similarity = best_similarity.max(normalized_edit_similarity(&query, &window));
        }
    }
    if best_similarity >= 750 {
        550_000 + best_similarity * 100
    } else {
        0
    }
}

fn normalize_search_text(value: &str) -> String {
    let mut normalized = String::new();
    let mut pending_space = false;
    for character in value
        .nfd()
        .flat_map(char::to_lowercase)
        .filter(|character| !is_combining_mark(*character))
    {
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

/// A thin catalog can omit artwork that Detail/Library already knows. Prefer
/// the last metadata header, then the saved entry, retaining the native preview
/// when neither contains a usable URL. Cache identity includes media type.
fn reuse_discover_poster(
    meta: &mut MetaPreview,
    header: Option<&MetaHeader>,
    entries: &[LibraryEntry],
) {
    let known = header
        .map(|h| h.poster_url.as_str())
        .filter(|url| !url.trim().is_empty())
        .or_else(|| {
            entries
                .iter()
                .find(|entry| entry.id == meta.id && entry.type_ == meta.type_)
                .map(|entry| entry.poster_url.as_str())
                .filter(|url| !url.trim().is_empty())
        });
    if let Some(url) = known {
        meta.poster = Some(url.into());
    }
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
        ..Default::default()
    }
}

/// Browse can supply genre/skip extras, but has no search query. Share the
/// same eligibility rule between its source picker and catalog definitions.
pub(crate) fn browsable_catalogs(manifest: &Manifest) -> impl Iterator<Item = &addons::Catalog> {
    manifest.catalogs.iter().filter(move |catalog| {
        (manifest.resources.is_empty() || manifest.accepts("catalog", &catalog.type_, &catalog.id))
            && !catalog
                .extra
                .iter()
                .any(|extra| extra.name == "search" && extra.is_required)
    })
}

pub(crate) fn build_type_defs(
    manifest: &Manifest,
    addon_label: &str,
    addon_url: &str,
) -> Vec<TypeDef> {
    use std::collections::HashMap;

    let mut by_type: HashMap<String, Vec<&addons::Catalog>> = HashMap::new();
    for cat in browsable_catalogs(manifest) {
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
                        addon_url: addon_url.into(),
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

    let active: Vec<&Installed> = discover_addon_indices(installed)
        .into_iter()
        .map(|index| &installed[index])
        .collect();

    // Collect all (type, catalog, addon_label) triples.
    type MergedCatalog = (String, String, bool, Vec<String>, String, String);
    let mut by_type: HashMap<String, Vec<MergedCatalog>> = HashMap::new();
    for inst in &active {
        for cat in browsable_catalogs(&inst.manifest) {
            by_type.entry(cat.type_.clone()).or_default().push((
                cat.id.clone(),
                cat.name.clone(),
                cat.supports_extra("skip"),
                catalog_genres(cat),
                inst.label.clone(),
                inst.url.clone(),
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
                .map(|(id, name, skip, genres, addon, owner)| {
                    let label = format!("{addon} — {name}");
                    CatDef {
                        id: id.clone(),
                        addon_url: owner.clone(),
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

    #[test]
    fn failed_search_pages_preserve_retry_cursor() {
        let mut target = search_target("retry");
        target.next_skip = 40;
        let mut targets = vec![target];
        record_search_page_outcome(&mut targets, 0, None);
        assert_eq!(targets[0].next_skip, 40);
        assert!(!targets[0].exhausted);
        record_search_page_outcome(
            &mut targets,
            0,
            Some(vec![search_meta("retried", "Retried")]),
        );
        assert_eq!(targets[0].next_skip, 41);
        assert_eq!(targets[0].results.len(), 1);
        assert!(!targets[0].exhausted);
        record_search_page_outcome(&mut targets, 0, Some(Vec::new()));
        assert_eq!(targets[0].next_skip, 41);
        assert!(targets[0].exhausted);
        targets[0].supports_skip = false;
        targets[0].exhausted = false;
        record_search_page_outcome(&mut targets, 0, None);
        assert!(
            !targets[0].exhausted,
            "non-paginated first-page failures can retry"
        );
        record_search_page_outcome(&mut targets, 0, Some(vec![search_meta("single", "Single")]));
        assert!(targets[0].exhausted);
        assert_eq!(targets[0].results.len(), 2);
    }

    #[test]
    fn debounced_search_preserves_a_space_until_the_next_word() {
        const CHILD: &str = "NOVA_TEST_SEARCH_SPACE_CHILD";
        let Ok(root) = std::env::var(CHILD) else {
            let root =
                std::env::temp_dir().join(format!("nova-search-space-{}", std::process::id()));
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app::catalog::tests::debounced_search_preserves_a_space_until_the_next_word",
                    "--nocapture",
                ])
                .env(CHILD, &root)
                .output()
                .unwrap();
            let _ = std::fs::remove_dir_all(&root);
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        storage::init_at(&root);
        let app = AppWindow::new().unwrap();
        app.window().show().unwrap();
        let player = crate::player::Player::setup(&app);
        let downloads = DownloadCoordinator::new(root.join("downloads"));
        #[cfg(feature = "desktop")]
        let bridge = {
            let (hi, _) = mpsc::channel();
            let (lo, _) = mpsc::channel();
            Bridge::new(
                app.as_weak(),
                PosterTx { hi, lo },
                Arc::new(AtomicU64::new(0)),
                player,
                downloads,
            )
        };
        #[cfg(not(feature = "desktop"))]
        let bridge = Bridge::new(
            app.as_weak(),
            Arc::new(AtomicU64::new(0)),
            player,
            downloads,
        );
        // Native picker callbacks can reselect the current row. Such events
        // must preserve the active filter and the page already loaded.
        {
            let mut state = bridge.shared.lock().unwrap();
            state.chosen_type = 0;
            state.chosen_catalog = 1;
            state.chosen_genre = "Drama".into();
            state.next_skip = 40;
            state.type_defs = vec![TypeDef {
                type_: "movie".into(),
                label: "Movies".into(),
                catalogs: vec![
                    CatDef {
                        id: "first".into(),
                        addon_url: String::new(),
                        label: "First".into(),
                        addon_label: String::new(),
                        genre_options: vec![],
                        supports_skip: true,
                    },
                    CatDef {
                        id: "second".into(),
                        addon_url: String::new(),
                        label: "Second".into(),
                        addon_label: String::new(),
                        genre_options: vec!["Drama".into()],
                        supports_skip: true,
                    },
                ],
            }];
        }
        bridge.pick_type("Movies");
        bridge.pick_catalog("Second");
        {
            let state = bridge.shared.lock().unwrap();
            assert_eq!(state.chosen_catalog, 1);
            assert_eq!(state.chosen_genre, "Drama");
            assert_eq!(state.next_skip, 40);
        }
        bridge.shared.lock().unwrap().next_skip = 0;
        bridge.shared.lock().unwrap().loading_more = true;
        bridge.catalog_gen.store(5, Ordering::Relaxed);
        bridge.load_more();
        assert_eq!(
            bridge.catalog_gen.load(Ordering::Relaxed),
            5,
            "page-zero retry must not duplicate an in-flight request"
        );
        bridge.shared.lock().unwrap().next_skip = 40;

        bridge.catalog_page_failed(4);
        assert!(bridge.shared.lock().unwrap().loading_more);
        bridge.catalog_page_failed(5);
        {
            let state = bridge.shared.lock().unwrap();
            assert!(!state.loading_more);
            assert!(!state.catalog_exhausted);
            assert_eq!(state.next_skip, 40);
        }
        for input in ["black ", "black clover"] {
            app.set_search_text(input.into());
            bridge.search_edited(input);
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
            assert_eq!(app.get_search_text(), input);
            assert_eq!(bridge.shared.lock().unwrap().search, input.trim());
        }
    }

    #[test]
    fn library_metadata_retries_follow_inventory_and_coalesce_pending_work() {
        let mut state = Shared::default();
        let now = std::time::Instant::now();
        assert!(reserve_metadata_prefetch(
            &mut state, "series", "saved", 1, now
        ));
        // Neither elapsed time nor a changed inventory may duplicate a job
        // which is already queued/fetching for this typed identity.
        assert!(!reserve_metadata_prefetch(
            &mut state,
            "series",
            "saved",
            1,
            now + Duration::from_secs(600)
        ));
        assert!(!reserve_metadata_prefetch(
            &mut state, "series", "saved", 2, now
        ));
        state
            .metadata_prefetch
            .get_mut(&("series".into(), "saved".into()))
            .unwrap()
            .retry_at = Some(now + Duration::from_secs(60));
        assert!(!reserve_metadata_prefetch(
            &mut state, "series", "saved", 1, now
        ));
        assert!(reserve_metadata_prefetch(
            &mut state, "series", "saved", 2, now
        ));
        state
            .metadata_prefetch
            .get_mut(&("series".into(), "saved".into()))
            .unwrap()
            .retry_at = Some(now + Duration::from_secs(60));
        assert!(reserve_metadata_prefetch(
            &mut state,
            "series",
            "saved",
            2,
            now + Duration::from_secs(60)
        ));
        assert!(reserve_metadata_prefetch(
            &mut state, "movie", "saved", 2, now
        ));
    }

    #[test]
    fn search_poster_snapshot_keeps_only_matching_sparse_visible_identities() {
        let previews = vec![
            MetaPreview {
                id: "first".into(),
                type_: "movie".into(),
                poster: Some("/first.png".into()),
                ..Default::default()
            },
            MetaPreview {
                id: "stale".into(),
                type_: "series".into(),
                poster: Some("/old.png".into()),
                ..Default::default()
            },
            MetaPreview {
                id: "visible".into(),
                type_: "series".into(),
                poster: Some("/visible.png".into()),
                ..Default::default()
            },
        ];
        let fetch = vec![
            // Reused header artwork can differ from the URL in the retained
            // preview; matching the current row identity must still reload it.
            (0, "/reused.png".into()),
            (1, "/new.png".into()),
            (2, "/visible.png".into()),
            (9, "/out-of-bounds.png".into()),
        ];
        let row_ids = HashMap::from([
            (0, "first".into()),
            (1, "stale".into()),
            (2, "different-row".into()),
            (9, "missing".into()),
        ]);

        assert_eq!(
            visible_search_poster_identities(&previews, &fetch, &row_ids),
            HashMap::from([
                (0, ("movie".into(), "first".into())),
                (1, ("series".into(), "stale".into())),
            ])
        );
    }

    fn search_target(id: &str) -> SearchTarget {
        SearchTarget {
            addon_url: format!("https://{id}.example/manifest.json"),
            type_: "movie".into(),
            catalog_id: id.into(),
            genre_options: Vec::new(),
            genre: String::new(),
            supports_skip: true,
            next_skip: 0,
            exhausted: false,
            results: Vec::new(),
        }
    }

    #[test]
    fn discover_reuses_detail_and_library_artwork_without_erasing_native_previews() {
        let mut meta = MetaPreview {
            id: "provider:show".into(),
            type_: "series".into(),
            poster: Some("https://images.example/catalog.jpg".into()),
            ..Default::default()
        };
        let entry: LibraryEntry = serde_json::from_value(serde_json::json!({
            "id":"provider:show", "type_":"series", "name":"Saved", "poster_url":"https://images.example/library.jpg"
        })).unwrap();
        reuse_discover_poster(&mut meta, None, std::slice::from_ref(&entry));
        assert_eq!(
            meta.poster.as_deref(),
            Some("https://images.example/library.jpg")
        );
        let header = MetaHeader {
            poster_url: "https://images.example/detail.jpg".into(),
            ..Default::default()
        };
        reuse_discover_poster(&mut meta, Some(&header), std::slice::from_ref(&entry));
        assert_eq!(
            meta.poster.as_deref(),
            Some("https://images.example/detail.jpg")
        );
        meta.poster = None;
        reuse_discover_poster(&mut meta, Some(&header), &[]);
        assert_eq!(
            meta.poster.as_deref(),
            Some("https://images.example/detail.jpg")
        );
        meta.type_ = "movie".into();
        meta.poster = Some("https://images.example/native.jpg".into());
        reuse_discover_poster(&mut meta, Some(&MetaHeader::default()), &[entry]);
        assert_eq!(
            meta.poster.as_deref(),
            Some("https://images.example/native.jpg")
        );
    }

    fn search_meta(id: &str, title: &str) -> MetaPreview {
        MetaPreview {
            id: id.into(),
            type_: "movie".into(),
            name: title.into(),
            ..Default::default()
        }
    }

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

    fn browse_inventory() -> Vec<Installed> {
        let mut required_search = extra("search", &[]);
        required_search.is_required = true;
        let mut addons = vec![
            installed(true, vec![]),
            installed(
                true,
                vec![catalog("series", "required", vec![required_search.clone()])],
            ),
            installed(
                true,
                vec![
                    catalog("series", "mixed-required", vec![required_search]),
                    catalog("movie", "optional", vec![extra("search", &[])]),
                ],
            ),
            installed(false, vec![catalog("series", "disabled", vec![])]),
            installed(true, vec![catalog("movie", "browse", vec![])]),
            installed(true, vec![catalog("series", "misdeclared", vec![])]),
        ];
        for (index, (addon, label)) in addons
            .iter_mut()
            .zip([
                "Streams",
                "Search only",
                "Optional search",
                "Disabled",
                "Browse",
                "Metadata",
            ])
            .enumerate()
        {
            addon.url = format!("http://127.0.0.1:1/{index}");
            addon.label = label.into();
        }
        addons[0].manifest.resources = vec![addons::Resource::Plain("stream".into())];
        addons[5].manifest.resources = vec![addons::Resource::Plain("meta".into())];
        addons
    }

    #[test]
    fn discover_browse_inventory_excludes_non_browsable_sources_but_preserves_search() {
        let addons = browse_inventory();
        assert_eq!(discover_addon_indices(&addons), [2, 4]);
        let merged = build_merged_type_defs(&addons);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].type_, "movie");
        assert_eq!(
            merged[0]
                .catalogs
                .iter()
                .map(|cat| cat.id.as_str())
                .collect::<Vec<_>>(),
            ["optional", "browse"]
        );
        assert!(build_type_defs(&addons[1].manifest, "Search only", &addons[1].url).is_empty());
        assert!(build_type_defs(&addons[5].manifest, "Metadata", &addons[5].url).is_empty());
        let single = build_type_defs(&addons[2].manifest, "Optional search", &addons[2].url);
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].catalogs[0].id, "optional");
        assert_eq!(
            build_search_targets(&addons)
                .iter()
                .map(|target| target.catalog_id.as_str())
                .collect::<Vec<_>>(),
            ["required", "mixed-required", "optional"]
        );
        let mut genre = extra("genre", &["Drama"]);
        genre.is_required = true;
        assert!(
            !build_type_defs(
                &installed(true, vec![catalog("movie", "genre", vec![genre])]).manifest,
                "Genres",
                "https://example.invalid"
            )
            .is_empty(),
            "a selectable required genre still permits browsing"
        );
    }

    #[test]
    fn discover_addon_picker_refresh_and_language_keep_browse_indices_and_search_only_sources() {
        const CHILD: &str = "NOVA_TEST_BROWSE_PICKER_CHILD";
        let Ok(root) = std::env::var(CHILD) else {
            let root =
                std::env::temp_dir().join(format!("nova-browse-picker-{}", std::process::id()));
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::catalog::tests::discover_addon_picker_refresh_and_language_keep_browse_indices_and_search_only_sources", "--nocapture"])
                .env(CHILD, &root).output().unwrap();
            let _ = std::fs::remove_dir_all(&root);
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        storage::init_at(&root);
        let app = AppWindow::new().unwrap();
        app.window().show().unwrap();
        let player = crate::player::Player::setup(&app);
        let downloads = DownloadCoordinator::new(root.join("downloads"));
        #[cfg(feature = "desktop")]
        let bridge = {
            let (hi, _) = mpsc::channel();
            let (lo, _) = mpsc::channel();
            Bridge::new(
                app.as_weak(),
                PosterTx { hi, lo },
                Arc::new(AtomicU64::new(0)),
                player,
                downloads,
            )
        };
        #[cfg(not(feature = "desktop"))]
        let bridge = Bridge::new(
            app.as_weak(),
            Arc::new(AtomicU64::new(0)),
            player,
            downloads,
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            state.installed = browse_inventory();
            state.chosen_addon = 2;
            // Skip unrelated Home fetches while testing picker publication.
            state.loading_addons = true;
        }
        let names = || {
            app.get_addon_names()
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>()
        };
        bridge.refresh_all(false);
        assert_eq!(names(), ["All addons", "Optional search", "Browse"]);
        assert_eq!(
            app.get_addon_combo_idx(),
            1,
            "picker index differs from installed index"
        );
        bridge.refresh_addon_picker_language_text();
        assert_eq!(names(), ["All addons", "Optional search", "Browse"]);
        assert_eq!(app.get_addon_combo_idx(), 1);
        assert!(
            app.get_search_addon_names()
                .iter()
                .any(|name| name == "Search only")
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            state.chosen_addon = 5;
        }
        bridge.refresh_all(false);
        assert_eq!(bridge.shared.lock().unwrap().chosen_addon, usize::MAX);
        assert_eq!(app.get_addon_combo_idx(), 0);
        assert_eq!(app.get_catalog_names().row_count(), 2);
        {
            let mut state = bridge.shared.lock().unwrap();
            state.installed[2].manifest.catalogs.clear();
            state.installed[4].enabled = false;
            state.chosen_addon = 2;
        }
        bridge.refresh_all(false);
        assert!(names().is_empty());
        assert_eq!(app.get_addon_combo_idx(), -1);
        assert_eq!(app.get_type_names().row_count(), 0);
        assert!(
            app.get_searchable(),
            "search-only catalogs remain usable with no browse sources"
        );
        bridge.refresh_addon_picker_language_text();
        bridge.refresh_catalog_language_text();
        assert!(names().is_empty());
        assert!(
            app.get_searchable(),
            "language refresh must preserve search-only support"
        );
        bridge.pick_addon("Search only");
        assert_eq!(
            bridge.shared.lock().unwrap().chosen_addon,
            usize::MAX,
            "stale callbacks cannot select a hidden browse source"
        );
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
    fn search_relevance_handles_word_order_spacing_accents_and_weak_partials() {
        let reordered = search_match_score("wars star", "Star Wars", None);
        let spacing = search_match_score("spiderman", "Spider Man", None);
        let accent = search_match_score("Amelie", "Amélie", None);
        let weak_partial = search_match_score("star wars", "Star Trek", None);
        let typo = search_match_score("satr wars", "Star Wars", None);

        assert!(reordered > typo);
        assert!(spacing > typo);
        assert_eq!(accent, 1_000_000);
        assert!(typo > weak_partial);
        assert_eq!(weak_partial, 0);
    }

    #[test]
    fn search_relevance_uses_year_but_does_not_fuzzy_match_numeric_tokens() {
        let exact_year = search_match_score("Movie 2020", "Movie", Some("2020"));
        let nearby_year = search_match_score("Movie 2021", "Movie", Some("2020"));
        assert!(exact_year > nearby_year);
    }

    #[test]
    fn partial_search_results_are_retained_and_stably_deduplicated() {
        let mut targets = vec![search_target("first"), search_target("second")];

        // The second source responds first. Its unique result is immediately
        // visible even though the earlier source has not returned yet.
        record_search_source_page(
            &mut targets,
            1,
            vec![
                search_meta("shared", "Common duplicate"),
                search_meta("second", "Common"),
            ],
        );
        let first_delivery = unique_search_results(&targets);
        assert_eq!(
            first_delivery
                .iter()
                .map(|meta| meta.id.as_str())
                .collect::<Vec<_>>(),
            ["shared", "second"]
        );

        // The preferred source arrives later. Its metadata wins the duplicate,
        // while both previously and newly delivered unique items remain.
        record_search_source_page(
            &mut targets,
            0,
            vec![
                search_meta("shared", "Common"),
                search_meta("first", "Common"),
            ],
        );
        let complete = ranked_search_results("Common", &targets);
        assert_eq!(
            complete
                .iter()
                .map(|meta| meta.id.as_str())
                .collect::<Vec<_>>(),
            ["shared", "first", "second"]
        );
        let unique = unique_search_results(&targets);
        assert_eq!(unique.len(), 3);
        assert_eq!(unique[0].title(), "Common");
        assert_eq!(targets[0].next_skip, 2);
    }

    #[test]
    fn search_history_restores_context_and_cascades_without_unrelated_sources() {
        let source = installed(
            true,
            vec![
                catalog(
                    "movie",
                    "saved",
                    vec![extra("search", &[]), extra("genre", &["Drama"])],
                ),
                catalog("series", "other", vec![extra("search", &[])]),
            ],
        );
        let context = SearchHistoryContext {
            catalog: Some((source.url.clone(), "movie".into(), "saved".into())),
            genre: "Drama".into(),
            ..Default::default()
        };
        let exact = restore_search_context(&context, std::slice::from_ref(&source));
        assert!(exact.catalog.is_some());
        assert_eq!(exact.genre, "Drama");
        let mut changed = source.clone();
        changed.manifest.catalogs[0].id = "replacement".into();
        let fallback = restore_search_context(&context, std::slice::from_ref(&changed));
        assert_eq!(fallback.addon_url, source.url);
        assert_eq!(fallback.type_, "movie");
        assert!(fallback.catalog.is_none());
        assert_eq!(fallback.genre, "Drama");
        changed.manifest.catalogs.remove(0);
        let stale_type = restore_search_context(&context, std::slice::from_ref(&changed));
        assert_eq!(stale_type.addon_url, source.url);
        assert!(stale_type.type_.is_empty());
        let stale_genre = SearchHistoryContext {
            genre: "Unknown genre".into(),
            ..context.clone()
        };
        assert!(
            restore_search_context(&stale_genre, std::slice::from_ref(&source))
                .genre
                .is_empty()
        );
        for available in [false, true] {
            changed.available = available;
            changed.enabled = !available;
            assert!(
                restore_search_context(&context, std::slice::from_ref(&changed))
                    == SearchFilters::default()
            );
        }
        let mut unrelated = source.clone();
        unrelated.url = "https://unrelated.example".into();
        assert!(restore_search_context(&context, &[unrelated]) == SearchFilters::default());
        assert!(restore_search_context(&context, &[]) == SearchFilters::default());
    }

    #[test]
    fn search_history_loads_legacy_on_missing_or_malformed_v2_but_respects_clear() {
        let legacy =
            || serde_json::from_str::<Vec<String>>(r#"["Dune","Star Wars","dune","a"]"#).unwrap();
        for current in [
            None,
            serde_json::from_str::<Vec<SearchHistoryEntry>>(r#"[{"query":4}]"#).ok(),
        ] {
            let loaded = normalize_search_history(current, legacy);
            assert_eq!(
                loaded
                    .iter()
                    .map(|entry| entry.query.as_str())
                    .collect::<Vec<_>>(),
                ["Dune", "Star Wars"]
            );
            assert!(
                loaded
                    .iter()
                    .all(|entry| entry.context == SearchHistoryContext::default())
            );
        }
        assert!(normalize_search_history(Some(Vec::new()), legacy).is_empty());
    }

    #[test]
    fn search_history_context_dedup_and_legacy_json_are_safe() {
        let global = SearchFilters::default();
        let scoped = SearchFilters {
            addon_url: "https://catalog.example".into(),
            genre: "Drama".into(),
            ..Default::default()
        };
        let mut history = Vec::new();
        remember_search_entry(&mut history, SearchHistoryEntry::new("Dune", &global, &[]));
        remember_search_entry(&mut history, SearchHistoryEntry::new("Dune", &scoped, &[]));
        assert_eq!(history.len(), 2);
        remember_search_entry(
            &mut history,
            SearchHistoryEntry::new(" dune ", &scoped, &[]),
        );
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].query, "dune");
        let encoded = serde_json::to_string(&history).unwrap();
        let decoded: Vec<SearchHistoryEntry> = serde_json::from_str(&encoded).unwrap();
        assert!(decoded[0].context == history[0].context);
        let legacy: Vec<String> = serde_json::from_str(r#"["Dune","Star Wars"]"#).unwrap();
        let migrated: Vec<_> = legacy
            .iter()
            .map(|query| SearchHistoryEntry::new(query, &global, &[]))
            .collect();
        assert!(
            migrated
                .iter()
                .all(|entry| entry.context == SearchHistoryContext::default())
        );
        let old_object: SearchHistoryEntry = serde_json::from_str(r#"{"query":"Dune"}"#).unwrap();
        assert!(old_object.context == SearchHistoryContext::default());
    }

    #[test]
    fn search_history_is_recent_first_unique_and_bounded() {
        fn remember_search_query(history: &mut Vec<String>, query: &str) {
            let mut entries: Vec<_> = history
                .iter()
                .map(|query| SearchHistoryEntry::new(query, &SearchFilters::default(), &[]))
                .collect();
            remember_search_entry(
                &mut entries,
                SearchHistoryEntry::new(query, &SearchFilters::default(), &[]),
            );
            *history = entries.into_iter().map(|entry| entry.query).collect();
        }
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
            addon_url: "https://example.invalid".into(),
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

//! Discover catalog: fetching, pagination, prefetching and pickers.
use super::*;
use nova_config::{CatalogGroupingKey, CatalogGroupingMode, CatalogSurface};
use nova_providers::catalog_grouping::SourceIdentity;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

const GROUPING_BACKFILL_PAGES: usize = 3;
const GROUPING_VISIBLE_TARGET: usize = 24;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PageStop {
    #[default]
    Ready,
    Empty,
    Repeated,
    Failed,
    Budget,
}

#[derive(Default)]
pub(super) struct PageState {
    key: Option<CatalogGroupingKey>,
    fingerprints: HashSet<String>,
    stop: PageStop,
    pages_in_wave: usize,
    visible_first: usize,
    visible_last: usize,
    mode: CatalogGroupingMode,
}

impl PageState {
    /// Bound intake retries to the reported viewport or its initial 32-card window.
    pub(super) fn poster_retry_range(&self, row_count: usize) -> std::ops::RangeInclusive<usize> {
        let first = self.visible_first.min(row_count.saturating_sub(1));
        let last = if self.visible_last > first {
            self.visible_last
        } else {
            first.saturating_add(31)
        };
        first
            ..=last
                .min(row_count.saturating_sub(1))
                .min(first.saturating_add(poster_intake::VISIBLE - 1))
    }

    fn accept(&mut self, key: CatalogGroupingKey, page: &[MetaPreview], append: bool) -> bool {
        if !append {
            *self = Self {
                key: Some(key),
                ..Default::default()
            };
        }
        self.pages_in_wave += 1;
        if page.is_empty() {
            self.stop = PageStop::Empty;
            return false;
        }
        let mut identities: Vec<_> = page.iter().map(catalog_source_identity).collect();
        identities.sort();
        identities.dedup();
        let fingerprint = nova_providers::catalog_grouping::projection_fingerprint(&identities);
        if !self.fingerprints.insert(fingerprint) {
            self.stop = PageStop::Repeated;
            return false;
        }
        self.stop = PageStop::Ready;
        true
    }

    fn resume(&mut self) -> bool {
        if !matches!(self.stop, PageStop::Budget | PageStop::Failed) {
            return false;
        }
        self.stop = PageStop::Ready;
        self.pages_in_wave = 0;
        true
    }

    fn should_backfill(
        &mut self,
        mode: CatalogGroupingMode,
        visible: usize,
        old_visible: usize,
        new_page: bool,
    ) -> bool {
        if self.mode != mode {
            self.mode = mode;
            if self.stop == PageStop::Budget {
                self.stop = PageStop::Ready;
            }
            self.pages_in_wave = self.pages_in_wave.min(1);
        }
        if mode == CatalogGroupingMode::Original
            || self.stop != PageStop::Ready
            || visible >= GROUPING_VISIBLE_TARGET
                && !(new_page && visible <= old_visible || !new_page && visible < old_visible)
        {
            return false;
        }
        if self.pages_in_wave >= GROUPING_BACKFILL_PAGES {
            self.stop = PageStop::Budget;
            false
        } else {
            true
        }
    }

    fn paused(&self) -> bool {
        self.stop == PageStop::Budget
    }
    fn terminal(&self) -> bool {
        matches!(self.stop, PageStop::Empty | PageStop::Repeated)
    }
}

fn selected_catalog_key(state: &Shared) -> Option<CatalogGroupingKey> {
    let type_ = state.type_defs.get(state.chosen_type)?;
    let catalog = type_.catalogs.get(state.chosen_catalog)?;
    Some(CatalogGroupingKey {
        addon_url: catalog.addon_url.clone(),
        type_: type_.type_.clone(),
        catalog_id: catalog.id.clone(),
        surface: CatalogSurface::Discover,
    })
}

fn catalog_source_identity(preview: &MetaPreview) -> SourceIdentity {
    SourceIdentity {
        addon_url: preview
            .extra
            .get("novaSourceUrl")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .into(),
        media_type: preview.type_.clone(),
        id: preview.id.clone(),
    }
}

fn append_catalog_raw(raw: &mut Vec<MetaPreview>, page: Vec<MetaPreview>) {
    let mut seen: HashSet<_> = raw.iter().map(catalog_source_identity).collect();
    raw.extend(
        page.into_iter()
            .filter(|preview| seen.insert(catalog_source_identity(preview))),
    );
}

#[derive(Clone)]
struct CatalogAnchor {
    index: usize,
    display_key: String,
    members: Vec<SourceIdentity>,
    season: Option<u32>,
}

fn locate_catalog_anchor(
    anchor: &CatalogAnchor,
    projected: &[(String, Vec<SourceIdentity>, Option<u32>)],
) -> usize {
    projected
        .iter()
        .position(|(key, _, _)| *key == anchor.display_key)
        .or_else(|| {
            projected.iter().position(|(_, members, season)| {
                *season == anchor.season
                    && members.iter().any(|member| anchor.members.contains(member))
            })
        })
        .or_else(|| {
            projected.iter().position(|(_, members, _)| {
                members.iter().any(|member| anchor.members.contains(member))
            })
        })
        .unwrap_or_else(|| anchor.index.min(projected.len().saturating_sub(1)))
}

const SAVED_EPISODE_BATCH: usize = 4;
const SAVED_EPISODE_BATCH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const SAVED_EPISODE_RECENT_INTERVAL: Duration = Duration::from_secs(30 * 60);
const SAVED_EPISODE_OLD_INTERVAL: Duration = Duration::from_secs(6 * 3600);
const SAVED_EPISODE_FAILURE_INTERVAL: Duration = Duration::from_secs(2 * 60);

type MetadataOwners = Vec<(String, u64)>;

struct SavedEpisodeRefresh {
    type_: String,
    id: String,
    sources: VecDeque<String>,
    interval: Duration,
}

fn metadata_owners(state: &Shared) -> MetadataOwners {
    state
        .installed
        .iter()
        .filter(|addon| addon.enabled && addon.available && addon.manifest.has_meta())
        .map(|addon| (addon.url.clone(), addon.generation))
        .collect()
}

fn reserve_saved_episode_refresh(
    state: &mut Shared,
    revision: u64,
    now: std::time::Instant,
    wall_now: u64,
) -> VecDeque<SavedEpisodeRefresh> {
    if state.restoring_models
        || state.loading_addons
        || state
            .saved_episode_refresh_next
            .is_some_and(|(rev, at)| rev == revision && now < at)
        || state
            .metadata_prefetch
            .values()
            .any(|job| job.retry_at.is_none())
    {
        return VecDeque::new();
    }
    let mut history = HashMap::<&str, u64>::new();
    for progress in state
        .progress
        .values()
        .filter(|p| p.watched || p.position_secs > 0.0 || p.play_count > 0)
    {
        history
            .entry(&progress.series_id)
            .and_modify(|at| *at = (*at).max(progress.updated_at_secs))
            .or_insert(progress.updated_at_secs);
    }
    let mut candidates = state
        .entries
        .iter()
        .filter_map(|entry| {
            if entry.type_ == "movie" || entry.type_.is_empty() || entry.id.is_empty() {
                return None;
            }
            let owner = profiles::ProfileStore::capture();
            let has_reminder = release_reminders::subscribed(&owner, &entry.type_, &entry.id);
            if !entry.watch_status.allows_episode_refresh(has_reminder) {
                return None;
            }
            let scheduled =
                has_reminder || smart_offline::enabled_title(&owner, &entry.type_, &entry.id);
            let last_watched = history
                .get(entry.id.as_str())
                .copied()
                .or_else(|| scheduled.then_some(wall_now))?;
            let previous = state
                .metadata_prefetch
                .get(&(entry.type_.clone(), entry.id.clone()));
            if previous.is_some_and(|job| {
                job.revision == revision && job.retry_at.is_none_or(|at| now < at)
            }) {
                return None;
            }
            let sources = state
                .installed
                .iter()
                .filter(|addon| {
                    addon.enabled
                        && addon.available
                        && addon.manifest.accepts("meta", &entry.type_, &entry.id)
                })
                .filter_map(|addon| Addon::new(&addon.url).ok())
                .map(|addon| addon.meta_url(&entry.type_, &entry.id))
                .collect::<VecDeque<_>>();
            if sources.is_empty() {
                return None;
            }
            let interval = if wall_now.saturating_sub(last_watched) < 30 * 24 * 3600 {
                SAVED_EPISODE_RECENT_INTERVAL
            } else {
                SAVED_EPISODE_OLD_INTERVAL
            };
            Some((
                previous.and_then(|job| job.retry_at),
                std::cmp::Reverse(last_watched),
                SavedEpisodeRefresh {
                    type_: entry.type_.clone(),
                    id: entry.id.clone(),
                    sources,
                    interval,
                },
            ))
        })
        .collect::<Vec<_>>();
    // Reserve half the budget for overdue recently watched titles. The other
    // half rotates through unvisited/oldest deadlines, so a large old library
    // neither blocks an active show's next refresh nor loses its own turn.
    candidates.sort_by_key(|(deadline, watched, job)| (*deadline, *watched, job.id.clone()));
    let mut urgent = Vec::new();
    let mut index = 0;
    while index < candidates.len() && urgent.len() < SAVED_EPISODE_BATCH / 2 {
        if candidates[index].0.is_some()
            && candidates[index].2.interval == SAVED_EPISODE_RECENT_INTERVAL
        {
            urgent.push(candidates.remove(index));
        } else {
            index += 1;
        }
    }
    urgent.extend(candidates);
    let candidates = urgent;
    let mut work = VecDeque::new();
    for (_, _, job) in candidates.into_iter().take(SAVED_EPISODE_BATCH) {
        if reserve_metadata_prefetch(state, &job.type_, &job.id, revision, now) {
            work.push_back(job);
        }
    }
    if !work.is_empty() {
        state.saved_episode_refresh_next = Some((revision, now + SAVED_EPISODE_BATCH_INTERVAL));
    }
    work
}

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
            state.catalog_page = PageState::default();
        }
        if let Some(app) = self.app() {
            app.set_loading_more(false);
            app.set_can_load_more(false);
            app.set_discover_pagination_paused(false);
            app.set_discover_pagination_hint(SharedString::default());
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
        if library {
            self.library_poster_visible_range(f, l);
            return;
        }
        if !library && !searching {
            {
                let mut state = self.shared.lock().unwrap();
                state.catalog_page.visible_first = f;
                state.catalog_page.visible_last = l;
            }
            self.schedule_visible_catalog_grouping();
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
            if state.loading_more || state.catalog_page.paused() {
                return;
            }
            state.catalog_page.pages_in_wave = 0;
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
        if profile_policy::restricted() && !profile_policy::management_browsing() {
            let previews = profile_policy::local_previews("");
            if let Some(app) = self.app() {
                app.set_catalog(
                    Rc::new(VecModel::from(
                        previews.iter().map(search_media_card).collect::<Vec<_>>(),
                    ))
                    .into(),
                );
                app.set_loading_more(false);
                app.set_can_load_more(false);
            }
            self.shared.lock().unwrap().previews = previews;
            return;
        }
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
                self.poster_intake.invalidate();
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
        let fetch_urls: Vec<(String, String)> = urls
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
                Some((
                    url.clone(),
                    addon.catalog_url(&type_, &catalog_id, &extra_refs),
                ))
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
        for (owner, url) in fetch_urls {
            let bridge = bridge.clone();
            let results = results.clone();
            let remaining = remaining.clone();
            let failed = failed.clone();
            net::fetch_bytes(url, move |result| {
                if let Ok(bytes) = result
                    && let Ok(mut metas) = Addon::parse_catalog(&bytes)
                {
                    for preview in &mut metas {
                        preview
                            .extra
                            .insert("novaSourceUrl".into(), serde_json::json!(owner));
                    }
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
        self.poster_intake.invalidate();
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
        {
            let mut state = self.shared.lock().unwrap();
            state.loading_more = false;
            state.catalog_page.stop = PageStop::Failed;
        }
        self.refresh_catalog_pagination_ui();
    }

    /// Main thread: raw page accounting precedes every display projection.
    pub(super) fn apply_catalog(&self, generation: u64, mut metas: Vec<MetaPreview>, append: bool) {
        if generation != self.catalog_gen.load(Ordering::Relaxed) {
            return;
        }
        let (key, accepted, old_visible_count) = {
            let mut state = self.shared.lock().unwrap();
            let Some(key) = selected_catalog_key(&state) else {
                return;
            };
            let old_visible_count = state.previews.len();
            for meta in &mut metas {
                if meta.type_.is_empty() {
                    meta.type_ = key.type_.clone();
                }
                // Ownership comes from the selected installed catalog, never
                // from provider-supplied display labels or extension claims.
                meta.extra
                    .insert("novaSourceUrl".into(), serde_json::json!(key.addon_url));
                reuse_discover_poster(
                    meta,
                    read_meta_header_for(&meta.type_, &meta.id).as_ref(),
                    &state.entries,
                );
            }
            let raw_count = metas.len();
            if !append {
                state.catalog_raw.clear();
                state.next_skip = 0;
            }
            let accepted = state.catalog_page.accept(key.clone(), &metas, append);
            state.next_skip = state.next_skip.saturating_add(raw_count);
            if accepted {
                append_catalog_raw(&mut state.catalog_raw, metas);
            }
            state.catalog_exhausted = state.catalog_page.terminal();
            state.catalog_gen = generation;
            state.loading_more = false;
            (key, accepted, old_visible_count)
        };
        if accepted || !append {
            self.publish_catalog_projection(&key, !append);
        }
        self.refresh_catalog_pagination_ui();
        if accepted {
            self.backfill_grouped_catalog(old_visible_count, true);
        }
        trim_heap();
    }

    pub(super) fn continue_catalog_pagination(&self) {
        {
            let mut state = self.shared.lock().unwrap();
            if state.loading_more || !state.catalog_page.resume() {
                return;
            }
        }
        self.load_more();
    }

    fn refresh_catalog_pagination_ui(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let (loading, can_load, paused, stop) = {
            let state = self.shared.lock().unwrap();
            let supports_skip = state
                .type_defs
                .get(state.chosen_type)
                .and_then(|type_| type_.catalogs.get(state.chosen_catalog))
                .is_some_and(|catalog| catalog.supports_skip);
            let paused = matches!(state.catalog_page.stop, PageStop::Budget | PageStop::Failed);
            (
                state.loading_more,
                supports_skip && !state.catalog_exhausted && !paused && !state.loading_more,
                paused,
                state.catalog_page.stop,
            )
        };
        app.set_loading_more(loading);
        app.set_can_load_more(can_load);
        app.set_discover_pagination_paused(paused);
        app.set_discover_pagination_hint(
            text::tr(match stop {
                PageStop::Budget => {
                    "More source pages are available. Continue browsing to load another batch."
                }
                PageStop::Failed => "This catalog page could not be loaded. Continue to retry.",
                PageStop::Repeated => "This addon repeated an earlier page.",
                _ => "",
            })
            .into(),
        );
    }

    fn capture_catalog_anchor(
        &self,
        previews: &[MetaPreview],
        index: usize,
    ) -> Option<CatalogAnchor> {
        let preview = previews.get(index)?;
        let context = self.catalog_grouping_display_context(preview);
        Some(CatalogAnchor {
            index,
            display_key: catalog_grouping::display_key(preview),
            members: context.as_ref().map_or_else(
                || vec![catalog_source_identity(preview)],
                |context| context.members.clone(),
            ),
            season: context.and_then(|context| context.canonical_season),
        })
    }

    fn publish_catalog_projection(&self, key: &CatalogGroupingKey, fresh: bool) {
        let Some(app) = self.app() else {
            return;
        };
        let (raw, old, first, generation) = {
            let state = self.shared.lock().unwrap();
            if state.catalog_page.key.as_ref() != Some(key)
                || selected_catalog_key(&state).as_ref() != Some(key)
            {
                return;
            }
            (
                state.catalog_raw.clone(),
                state.previews.clone(),
                state.catalog_page.visible_first,
                self.catalog_gen.load(Ordering::Relaxed),
            )
        };
        // Snapshot old membership before common projection replaces its action registry.
        let focus = (!fresh)
            .then(|| self.capture_catalog_anchor(&old, app.get_discover_kb_idx().max(0) as usize))
            .flatten();
        let viewport = (!fresh)
            .then(|| self.capture_catalog_anchor(&old, first))
            .flatten();
        let old_model = app.get_catalog();
        let old_pixels: Vec<_> = old
            .iter()
            .enumerate()
            .filter_map(|(index, preview)| {
                old_model
                    .row_data(index)
                    .filter(|card| card.is_loaded)
                    .map(|card| {
                        (
                            catalog_source_identity(preview),
                            catalog_grouping::canonical_season(preview),
                            preview.poster.clone().unwrap_or_default(),
                            card,
                        )
                    })
            })
            .collect();
        let projected = self.project_catalog_grouping(key, &raw);
        let contexts: Vec<_> = projected
            .iter()
            .map(|preview| {
                let context = self.catalog_grouping_display_context(preview);
                (
                    catalog_grouping::display_key(preview),
                    context.as_ref().map_or_else(
                        || vec![catalog_source_identity(preview)],
                        |context| context.members.clone(),
                    ),
                    context.and_then(|context| context.canonical_season),
                )
            })
            .collect();
        let cards: Vec<_> = projected
            .iter()
            .map(|preview| {
                let source = catalog_source_identity(preview);
                let season = catalog_grouping::canonical_season(preview);
                let url = preview.poster.clone().unwrap_or_default();
                let retained =
                    old_pixels
                        .iter()
                        .find(|(identity, previous_season, previous_url, _)| {
                            *identity == source
                                && *previous_url == url
                                && (*previous_season == season
                                    || previous_season.is_none()
                                    || season.is_none())
                        });
                MediaCard {
                    id: preview.id.as_str().into(),
                    title: preview.title().into(),
                    year: preview.year_str().unwrap_or_default().into(),
                    poster_path: url.into(),
                    poster: retained
                        .map_or_else(Image::default, |(_, _, _, card)| card.poster.clone()),
                    is_loaded: retained.is_some(),
                    badge: season.map_or_else(SharedString::default, |season| {
                        text::season_label(season).into()
                    }),
                    ..Default::default()
                }
            })
            .collect();
        let len = projected.len();
        {
            let mut state = self.shared.lock().unwrap();
            state.previews = projected.clone();
        }
        app.set_catalog(Rc::new(VecModel::from(cards)).into());
        app.set_empty_hint(text::tr("No results for this selection.").into());
        if let Some(focus) = focus {
            app.set_discover_kb_idx(locate_catalog_anchor(&focus, &contexts) as i32);
        }
        if let Some(viewport) = viewport {
            let mapped = locate_catalog_anchor(&viewport, &contexts);
            self.shared.lock().unwrap().catalog_page.visible_first = mapped;
            app.global::<nova_ui::DiscoverBridge>()
                .invoke_remap_catalog_anchor(viewport.index as i32, mapped as i32);
        }
        let mut background_saturated = false;
        for (index, preview) in projected.iter().enumerate() {
            if let Some(_url) = preview.poster.as_ref().filter(|url| !url.trim().is_empty()) {
                if app
                    .get_catalog()
                    .row_data(index)
                    .is_some_and(|card| !card.is_loaded)
                    && !background_saturated
                {
                    background_saturated = matches!(
                        self.queue_poster_intake(
                            poster_intake::Surface::Catalog,
                            index,
                            false,
                            String::new(),
                        ),
                        poster_intake::Admission::DeferredCapacity
                            | poster_intake::Admission::Closed
                            | poster_intake::Admission::Obsolete
                    );
                }
            } else {
                self.recover_catalog_artwork(generation, index, "");
            }
        }
        // Refresh promotes the current viewport before the first background
        // request is dispatched, and pumps the admitted batch once.
        self.refresh_poster_intake();
        if fresh && active_cache_settings().prefetch_metadata {
            self.prefetch_background_meta(&projected, generation);
        }
        self.schedule_visible_catalog_grouping();
        crate::web_log(&format!("nova: catalog projected {len} cards"));
    }

    fn schedule_visible_catalog_grouping(&self) {
        let (key, candidates) = {
            let state = self.shared.lock().unwrap();
            let Some(key) = state
                .catalog_page
                .key
                .clone()
                .filter(|key| selected_catalog_key(&state).as_ref() == Some(key))
            else {
                return;
            };
            let first = state.catalog_page.visible_first.min(state.previews.len());
            let last = state
                .catalog_page
                .visible_last
                .saturating_add(1)
                .max(first.saturating_add(GROUPING_VISIBLE_TARGET))
                .min(state.previews.len());
            let visible: HashSet<_> = state.previews[first..last]
                .iter()
                .map(catalog_source_identity)
                .collect();
            let candidates = state
                .catalog_raw
                .iter()
                .filter(|preview| visible.contains(&catalog_source_identity(preview)))
                .take(48)
                .cloned()
                .collect::<Vec<_>>();
            (key, candidates)
        };
        self.schedule_catalog_grouping(key, &candidates);
    }

    /// Rebuild only the derived grid. Raw source order, cursor and selection survive.
    pub(super) fn reproject_catalog_grouping_discover(&self) {
        let (key, old_count) = {
            let state = self.shared.lock().unwrap();
            let Some(key) = state
                .catalog_page
                .key
                .clone()
                .filter(|key| selected_catalog_key(&state).as_ref() == Some(key))
            else {
                return;
            };
            (key, state.previews.len())
        };
        self.publish_catalog_projection(&key, false);
        self.refresh_catalog_pagination_ui();
        self.backfill_grouped_catalog(old_count, false);
    }

    fn backfill_grouped_catalog(&self, old_visible_count: usize, new_page: bool) {
        let fetch = {
            let mut state = self.shared.lock().unwrap();
            let Some(key) = state.catalog_page.key.clone() else {
                return;
            };
            let mode = state.cache_settings.catalog_grouping_mode(&key);
            let supports_skip = state
                .type_defs
                .get(state.chosen_type)
                .and_then(|type_| type_.catalogs.get(state.chosen_catalog))
                .is_some_and(|catalog| catalog.supports_skip);
            if state.loading_more
                || !supports_skip
                || state.catalog_exhausted
                || state.catalog_raw.is_empty()
            {
                false
            } else {
                let visible = state.previews.len();
                state
                    .catalog_page
                    .should_backfill(mode, visible, old_visible_count, new_page)
            }
        };
        self.refresh_catalog_pagination_ui();
        if fetch {
            self.fetch_catalog_page(true);
        }
    }

    #[cfg(test)]
    pub(super) fn catalog_poster_slot_matches(
        &self,
        generation: u64,
        index: usize,
        key: &str,
        url: &str,
    ) -> bool {
        generation == self.catalog_gen.load(Ordering::Relaxed)
            && self
                .shared
                .lock()
                .unwrap()
                .previews
                .get(index)
                .is_some_and(|preview| {
                    catalog_grouping::display_key(preview) == key
                        && preview.poster.as_deref().unwrap_or_default() == url
                })
    }

    /// Queue only the current visible window. Capacity retries always inspect
    /// the current model/profile rather than a releasing request's old viewport.
    pub(super) fn queue_visible_catalog_posters(&self) {
        let Some(app) = self.app() else { return };
        if app.get_show_home()
            || app.get_show_library()
            || app.get_more_open()
            || app.get_show_settings()
            || app.get_show_about()
            || app.get_show_calendar()
            || app.get_show_downloads()
            || app.get_player_open()
            || app.get_modal_visible()
            || app.get_profiles_require_selection()
            || app.get_discover_search_open()
        {
            return;
        }
        let _timing = perf::Timer::new("poster.visible_retry");
        let model = app.get_catalog();
        if model.row_count() == 0 {
            return;
        }
        let range = self
            .shared
            .lock()
            .unwrap()
            .catalog_page
            .poster_retry_range(model.row_count());
        let center = range.start() + (range.end() - range.start()) / 2;
        let mut indices = range.collect::<Vec<_>>();
        indices.sort_by_key(|index| index.abs_diff(center));
        for (index, _) in indices
            .into_iter()
            .filter_map(|index| model.row_data(index).map(|row| (index, row)))
            .filter(|(_, row)| !row.is_loaded && !row.poster_path.is_empty())
        {
            if matches!(
                self.queue_poster_intake(
                    poster_intake::Surface::Catalog,
                    index,
                    true,
                    String::new(),
                ),
                poster_intake::Admission::DeferredCapacity | poster_intake::Admission::Closed
            ) {
                break;
            }
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
                    bridge.request_progress_projection();
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
                } else {
                    // Try the next addon URL for this pair.
                    work.push_front((type_, id, urls));
                }
                // Admission rejection can complete synchronously. Post the next
                // request so a large library cannot recurse through all pairs.
                // Parsing and cache writes above remain on the fetch worker.
                let remaining = Arc::new(Mutex::new(Some(work)));
                let queued = remaining.clone();
                let next = bridge2.clone();
                if profiles::invoke_from_event_loop(move || {
                    let work = queued.lock().unwrap().take().unwrap_or_default();
                    run(next, work, cached, revision);
                })
                .is_err()
                {
                    // No continuation will run: retire reservations instead of
                    // leaving this owner's prefetch rows permanently pending.
                    if let Some(work) = remaining.lock().unwrap().take() {
                        for (type_, id, _) in work {
                            bridge2.finish_metadata_prefetch(&type_, &id, false);
                        }
                    }
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
            self.persist_backdrop_for(type_, id, &header.background_url);
        }
        self.persist_header_for(
            type_,
            id,
            &header.genres,
            &header.description,
            &header.year,
            false,
        );
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
        let (entries, displayed) = {
            let mut state = self.shared.lock().unwrap();
            if state.library_prefetch_preparing || state.restoring_models {
                return;
            }
            state.library_prefetch_preparing = true;
            (
                state
                    .entries
                    .iter()
                    .map(|e| {
                        (
                            e.type_.clone(),
                            e.id.clone(),
                            !e.description.is_empty() || !e.genres.is_empty(),
                        )
                    })
                    .collect::<Vec<_>>(),
                state.library_displayed.clone(),
            )
        };
        let bridge = self.clone();
        let context = profiles::capture();
        thread::spawn(move || {
            let order = displayed
                .into_iter()
                .enumerate()
                .map(|(i, key)| (key, i))
                .collect::<HashMap<_, _>>();
            let mut entries = entries;
            entries.sort_by_key(|(type_, id, _)| {
                order
                    .get(&(type_.clone(), id.clone()))
                    .copied()
                    .unwrap_or(usize::MAX)
            });
            let mut pairs = entries
                .into_iter()
                .filter_map(|(type_, id, text)| {
                    if !profiles::is_current(&context) || type_.is_empty() || id.is_empty() {
                        return None;
                    }
                    let has_text = text || header_text_cached(&type_, &id);
                    let has_episodes = type_ == "movie"
                        || read_episodes_cache_for(&type_, &id).is_some_and(|v| !v.is_empty());
                    (!(has_text && has_episodes)).then_some((type_, id))
                })
                .collect::<Vec<_>>();
            let saved = {
                let mut state = bridge.shared.lock().unwrap();
                state.library_prefetch_preparing = false;
                state
                    .entries
                    .iter()
                    .map(|e| (e.type_.clone(), e.id.clone()))
                    .collect::<HashSet<_>>()
            };
            pairs.retain(|key| saved.contains(key));
            if profiles::is_current(&context) {
                bridge.prefetch_meta_pairs(pairs);
            }
        });
    }

    /// Revalidate watched saved series even when their cache looks complete.
    /// Completion/Ended is derived from that same inventory and cannot be used
    /// to decide that a title will never gain a new season.
    pub(super) fn refresh_saved_episode_metadata(&self) {
        let Some(app) = self.app() else {
            return;
        };
        if !remote_playback::is_foreground()
            || !app.window().is_visible()
            || app.window().is_minimized()
            || (!app.get_show_home() && !app.get_show_library() && !app.get_show_calendar())
            || app.get_player_open()
            || app.get_modal_visible()
        {
            return;
        }
        {
            let mut state = self.shared.lock().unwrap();
            if state.saved_episode_refresh_preparing
                || state.restoring_models
                || state.saved_episode_refresh_next.is_some_and(|(rev, at)| {
                    rev == nova_providers::metadata_revision() && std::time::Instant::now() < at
                })
                || state
                    .metadata_prefetch
                    .values()
                    .any(|job| job.retry_at.is_none())
            {
                return;
            }
            state.saved_episode_refresh_preparing = true;
        }
        let bridge = self.clone();
        let context = profiles::capture();
        thread::spawn(move || {
            let version = progress_projection::revision();
            let revision = nova_providers::metadata_revision();
            let current =
                || profiles::is_current(&context) && progress_projection::revision() == version;
            let mut prepared = progress_projection::snapshot(&bridge, current);
            if let Some(data) = prepared.as_mut() {
                let state = bridge.shared.lock().unwrap();
                data.installed = state.installed.clone();
                data.metadata_prefetch = state.metadata_prefetch.clone();
                data.saved_episode_refresh_next = state.saved_episode_refresh_next;
                data.loading_addons = state.loading_addons;
            }
            let owners = prepared.as_ref().map(metadata_owners).unwrap_or_default();
            let work = prepared
                .as_mut()
                .map(|data| {
                    reserve_saved_episode_refresh(
                        data,
                        revision,
                        std::time::Instant::now(),
                        now_secs(),
                    )
                })
                .unwrap_or_default();
            let _ = profiles::invoke_from_event_loop(move || {
                let accepted = {
                    let mut state = bridge.shared.lock().unwrap();
                    state.saved_episode_refresh_preparing = false;
                    if progress_projection::revision() != version
                        || nova_providers::metadata_revision() != revision
                        || metadata_owners(&state) != owners
                    {
                        drop(state);
                        bridge.refresh_saved_episode_metadata();
                        return;
                    }
                    // Claim only the bounded prepared batch against current
                    // reservations; a concurrent missing-metadata pass wins.
                    let mut accepted = VecDeque::new();
                    if !state
                        .metadata_prefetch
                        .values()
                        .any(|job| job.retry_at.is_none())
                    {
                        for job in work {
                            if state
                                .entries
                                .iter()
                                .any(|e| e.id == job.id && e.type_ == job.type_)
                                && reserve_metadata_prefetch(
                                    &mut state,
                                    &job.type_,
                                    &job.id,
                                    revision,
                                    std::time::Instant::now(),
                                )
                            {
                                accepted.push_back(job);
                            }
                        }
                    }
                    if !accepted.is_empty() {
                        state.saved_episode_refresh_next = Some((
                            revision,
                            std::time::Instant::now() + SAVED_EPISODE_BATCH_INTERVAL,
                        ));
                    }
                    accepted
                };
                bridge.run_saved_episode_refresh(accepted, owners, revision);
            });
        });
    }

    fn finish_saved_episode_refresh(&self, job: &SavedEpisodeRefresh, success: bool) {
        let owner = profiles::ProfileStore::capture();
        if release_reminders::subscribed(&owner, &job.type_, &job.id) {
            release_reminders::note_metadata_refresh(
                &owner,
                if success {
                    Ok(())
                } else {
                    Err(text::tr("Metadata refresh pending; cached schedule retained").into())
                },
            );
        }
        if let Some(prefetch) = self
            .shared
            .lock()
            .unwrap()
            .metadata_prefetch
            .get_mut(&(job.type_.clone(), job.id.clone()))
        {
            prefetch.retry_at = Some(
                std::time::Instant::now()
                    + if success {
                        job.interval
                    } else {
                        SAVED_EPISODE_FAILURE_INTERVAL
                    },
            );
        }
    }

    fn run_saved_episode_refresh(
        &self,
        mut work: VecDeque<SavedEpisodeRefresh>,
        owners: MetadataOwners,
        revision: u64,
    ) {
        let Some(mut job) = work.pop_front() else {
            return;
        };
        let current = {
            let state = self.shared.lock().unwrap();
            remote_playback::is_foreground()
                && self.app().is_some_and(|app| {
                    app.window().is_visible()
                        && !app.window().is_minimized()
                        && (app.get_show_home()
                            || app.get_show_library()
                            || app.get_show_calendar())
                        && !app.get_player_open()
                        && !app.get_modal_visible()
                })
                && metadata_owners(&state) == owners
                && revision == nova_providers::metadata_revision()
        };
        if !current {
            self.finish_saved_episode_refresh(&job, false);
            for pending in &work {
                self.finish_saved_episode_refresh(pending, false);
            }
            self.shared.lock().unwrap().saved_episode_refresh_next = None;
            return;
        }
        let eligible = self.shared.lock().unwrap().entries.iter().any(|entry| {
            entry.id == job.id
                && entry.type_ == job.type_
                && entry
                    .watch_status
                    .allows_episode_refresh(release_reminders::subscribed(
                        &profiles::ProfileStore::capture(),
                        &entry.type_,
                        &entry.id,
                    ))
        });
        let Some(url) = job.sources.pop_front().filter(|_| eligible) else {
            self.finish_saved_episode_refresh(&job, false);
            self.run_saved_episode_refresh(work, owners, revision);
            return;
        };
        // Detail or another metadata request may publish while this slower
        // background fetch is in flight. Keep that newer inventory intact.
        let baseline = storage::get_str(&episodes_key(&job.type_, &job.id));
        let bridge = self.clone();
        net::fetch_bytes(url, move |result| {
            let item = result
                .ok()
                .and_then(|bytes| Addon::parse_meta(&bytes).ok().flatten())
                .filter(|item| meta_matches_request(item, &job.type_, &job.id));
            // Publication is UI-thread/profile gated: old profile completions
            // cannot mutate replacement Shared state or its caches. Validate
            // the source generation/inventory again after the HTTP/broker work.
            let _ = profiles::invoke_from_event_loop(move || {
                let current = {
                    let state = bridge.shared.lock().unwrap();
                    metadata_owners(&state) == owners
                        && revision == nova_providers::metadata_revision()
                        && storage::get_str(&episodes_key(&job.type_, &job.id)) == baseline
                        && state.entries.iter().any(|entry| {
                            entry.id == job.id
                                && entry.type_ == job.type_
                                && entry.watch_status.allows_episode_refresh(
                                    release_reminders::subscribed(
                                        &profiles::ProfileStore::capture(),
                                        &entry.type_,
                                        &entry.id,
                                    ),
                                )
                        })
                };
                if !current {
                    bridge.finish_saved_episode_refresh(&job, false);
                    bridge.run_saved_episode_refresh(work, owners, revision);
                    return;
                }
                if item
                    .as_ref()
                    .is_some_and(|item| bridge.cache_background_meta(&job.type_, &job.id, item) > 0)
                {
                    bridge.finish_saved_episode_refresh(&job, true);
                    bridge.request_progress_projection();
                } else {
                    // A failed/empty source must not replace the known episode
                    // list; try the next accepting addon within the same slot.
                    work.push_front(job);
                }
                bridge.run_saved_episode_refresh(work, owners, revision);
            });
        });
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
            state.search = text.trim().to_owned();
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

        self.refresh_local_search(text);

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
            state.search_targets =
                if profile_policy::restricted() && !profile_policy::management_browsing() {
                    vec![SearchTarget {
                        addon_url: String::new(),
                        type_: String::new(),
                        catalog_id: String::new(),
                        genre_options: Vec::new(),
                        genre: String::new(),
                        supports_skip: false,
                        next_skip: 0,
                        exhausted: true,
                        page_fingerprints: HashSet::new(),
                        results: profile_policy::local_previews(&trimmed).into(),
                    }]
                } else {
                    filtered_search_targets(&state.installed, &state.search_filters)
                };
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

        self.refresh_local_search(&trimmed);
        if target_count == 0
            || profile_policy::restricted() && !profile_policy::management_browsing()
        {
            self.remember_search(&trimmed);
        }

        if profile_policy::restricted() && !profile_policy::management_browsing() {
            self.apply_search_results(generation, Vec::new(), false);
            return;
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
        let work: Vec<(usize, String, String, SearchSourceReceipt)> = {
            let mut state = self.shared.lock().unwrap();
            if generation != state.search_generation {
                return;
            }
            let work = target_indices
                .into_iter()
                .filter_map(|index| {
                    let target = state.search_targets.get(index)?;
                    let installed = state.installed.iter().find(|source| {
                        source.url == target.addon_url && source.enabled && source.available
                    })?;
                    let source = SearchSourceReceipt {
                        url: installed.url.clone(),
                        generation: installed.generation,
                        enabled: installed.enabled,
                        available: installed.available,
                    };
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
                        source,
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

        for (target_index, url, type_, source) in work {
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
                    let authority_current =
                        bridge.shared.lock().unwrap().installed.iter().any(|now| {
                            now.url == source.url
                                && now.generation == source.generation
                                && now.enabled == source.enabled
                                && now.available == source.available
                        });
                    bridge.apply_search_results(
                        generation,
                        vec![(target_index, if authority_current { page } else { None })],
                        append,
                    );
                });
            });
        }
    }

    #[cfg(test)]
    pub(super) fn search_test_admit_page(&self, generation: u64, page: Vec<MetaPreview>) {
        self.apply_search_results(generation, vec![(0, Some(page))], false);
    }

    pub(super) fn repaint_search(&self) {
        let generation = self.shared.lock().unwrap().search_generation;
        self.apply_search_results(generation, Vec::new(), false);
    }

    /// Apply each completed source page immediately, preserving deterministic
    /// source ordering and independent cursors for catalogs that support skip.
    fn apply_search_results(
        &self,
        generation: u64,
        pages: Vec<(usize, Option<Vec<MetaPreview>>)>,
        append: bool,
    ) {
        #[cfg(test)]
        if std::env::var_os("NOVA_UI05_LEGACY").is_some() {
            self.apply_search_results_legacy(generation, pages, append);
            return;
        }
        #[cfg(test)]
        let began = std::time::Instant::now();
        #[cfg(test)]
        let allocation_scope = search_publication_bench::AllocationScope::begin();
        #[cfg(test)]
        let (ingress_wait_ns, ingress_hold_ns);
        let context = profiles::capture();
        if !profiles::is_current(&context) {
            return;
        }
        let (mut input, retired, needs_retraction) = {
            #[cfg(test)]
            let lock_waiting = std::time::Instant::now();
            let mut state = self.shared.lock().unwrap();
            #[cfg(test)]
            {
                ingress_wait_ns = lock_waiting.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            }
            #[cfg(test)]
            let lock_held = std::time::Instant::now();
            if state.search_generation != generation {
                return;
            }
            let stamp = state
                .unified_search
                .presentation_stamp(self.downloads.clone());
            let local = state.unified_search.projection_local();
            let sources = state
                .installed
                .iter()
                .map(|source| SearchSourceReceipt {
                    url: source.url.clone(),
                    generation: source.generation,
                    enabled: source.enabled,
                    available: source.available,
                })
                .collect();
            let history_entry =
                SearchHistoryEntry::new(&state.search, &state.search_filters, &state.installed);
            let pending_outcomes = state.search_pending_requests;
            let query = state.search.clone();
            let targets = state.search_targets.clone();
            let Shared {
                entries,
                search_projection: view,
                ..
            } = &mut *state;
            view.capture_entries(entries);
            let needs_retraction = !view.frame.keys.is_empty()
                && (profile_policy::entry_blocked()
                    || view
                        .policy_stamp
                        .as_ref()
                        .is_some_and(|prior| prior.visibility_key() != stamp.visibility_key())
                    || view
                        .published_scope
                        .is_some_and(|scope| scope != local.scope)
                    || !view.last_sources.is_empty() && view.last_sources != sources);
            let mut retired = None;
            if view.generation != generation {
                view.generation = generation;
                view.completed_outcomes = 0;
                view.model_revision = view.model_revision.wrapping_add(1);
                retired = Some(std::mem::take(&mut view.frame));
            }
            view.requested = view.requested.wrapping_add(1);
            let captured = (
                SearchProjectionRequest {
                    context,
                    bridge: self.clone(),
                    generation,
                    query,
                    local,
                    stamp,
                    targets,
                    sources,
                    pages,
                    discarded_outcomes: 0,
                    sequence: view.requested,
                    memory_revision: view.memory_revision,
                    model_revision: view.model_revision,
                    base_entries: view.base_entries.clone(),
                    entry_deltas: view.entry_deltas.clone(),
                    frame: view.frame.clone(),
                    append,
                    #[cfg(test)]
                    ingress: Arc::new(AtomicU64::new(0)),
                    #[cfg(test)]
                    ingress_details: Arc::new(Mutex::new(SearchIngressDetails::default())),
                    pending_outcomes,
                    applied_outcomes: view.completed_outcomes,
                    history_entry,
                },
                retired,
                needs_retraction,
            );
            #[cfg(test)]
            {
                ingress_hold_ns = lock_held.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            }
            captured
        };
        if needs_retraction {
            self.retract_search_projection();
            let state = self.shared.lock().unwrap();
            input.frame = state.search_projection.frame.clone();
            input.model_revision = state.search_projection.model_revision;
        }
        #[cfg(test)]
        let ingress = input.ingress.clone();
        #[cfg(test)]
        let ingress_details = input.ingress_details.clone();
        self.enqueue_search_projection(input);
        drop(retired);
        #[cfg(test)]
        ingress.store(
            u64::try_from(began.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Release,
        );
        #[cfg(test)]
        {
            *ingress_details.lock().unwrap() = SearchIngressDetails {
                allocator: allocation_scope.finish(),
                wait_ns: ingress_wait_ns,
                hold_ns: ingress_hold_ns,
            };
        }
    }

    pub(super) fn process_search_projection(
        &self,
        task: (SearchProjectionRequest, SearchProjectionProcessor),
        work: Arc<Mutex<unified_search::SearchWork>>,
    ) {
        #[cfg(test)]
        let preparation_began = std::time::Instant::now();
        #[cfg(test)]
        let preparation_allocations = search_publication_bench::AllocationScope::begin();
        let (request, mut processor) = task;
        let context = request.context.clone();
        let prepared = profiles::with_context(context, || {
            let current = || {
                profiles::is_current(&request.context)
                    && work.lock().unwrap().projection_current(&request)
            };
            let query_current = || {
                profiles::is_current(&request.context)
                    && work.lock().unwrap().projection_query_current(&request)
            };
            processor.prepare(request.clone(), current, query_current)
        });
        drop(request);
        #[cfg(test)]
        let prepared = prepared.map(|mut prepared| {
            prepared.stats.elapsed_ns =
                preparation_began.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            prepared.stats.allocator = preparation_allocations.finish();
            prepared
        });
        work.lock().unwrap().finish_projection(processor);
        if let Some(prepared) = prepared {
            let bridge = prepared.request.bridge.clone();
            let _ = profiles::with_context(prepared.request.context.clone(), || {
                profiles::invoke_from_event_loop(move || {
                    bridge.publish_search_projection(prepared, work)
                })
            });
        }
    }

    fn publish_search_projection(
        &self,
        mut prepared: PreparedSearchProjection,
        work: Arc<Mutex<unified_search::SearchWork>>,
    ) {
        #[cfg(test)]
        let _sample = search_publication_bench::Callback::begin();
        let Some(app) = self.app() else { return };
        if !profiles::is_current(&prepared.request.context)
            || !work.lock().unwrap().projection_current(&prepared.request)
        {
            return;
        }
        let request = &prepared.request;
        if profile_policy::entry_blocked() {
            self.retract_search_projection();
            return;
        }
        let (basis, needs_replan) = {
            #[cfg(test)]
            let lock_sample = search_publication_bench::SharedLock::waiting();
            let mut state = self.shared.lock().unwrap();
            #[cfg(test)]
            let _lock_sample = lock_sample.acquired();
            if state.search_generation != request.generation
                || state.search != request.query
                || state.unified_search.projection_local().scope != request.local.scope
            {
                return;
            }
            let sources: Vec<_> = state
                .installed
                .iter()
                .map(|source| SearchSourceReceipt {
                    url: source.url.clone(),
                    generation: source.generation,
                    enabled: source.enabled,
                    available: source.available,
                })
                .collect();
            if sources != request.sources {
                return;
            }
            {
                let Shared {
                    entries,
                    search_projection,
                    ..
                } = &mut *state;
                search_projection.capture_entries(entries);
            }
            (
                state.search_projection.frame.clone(),
                state.search_projection.model_revision != request.model_revision
                    || state.search_projection.memory_revision != request.memory_revision,
            )
        };
        if needs_replan {
            self.repaint_search();
            return;
        }
        let model = app.get_search_results();
        if model
            .as_any()
            .downcast_ref::<VecModel<MediaCard>>()
            .is_none()
        {
            // The initial empty Slint property need not be a VecModel. Install
            // its mutable model once; retained search delegates keep theirs.
            let initial = model.row_count() == 0 && basis.keys.is_empty();
            app.set_search_results(Rc::new(VecModel::<MediaCard>::from(vec![])).into());
            if !initial {
                self.retract_search_projection();
                self.repaint_search();
                return;
            }
        }
        let model = app.get_search_results();
        if model.row_count() != basis.keys.len() {
            self.retract_search_projection();
            self.repaint_search();
            return;
        }
        #[cfg(test)]
        let card_sample = search_publication_bench::Phase::begin("cards");
        let mut old = HashMap::new();
        for (index, key) in basis.keys.iter().enumerate() {
            if let Some(row) = model.row_data(index) {
                old.insert(key.clone(), (index, row));
            }
        }
        let focused = (app.get_kb_active() && app.get_discover_kb_zone() == 3)
            .then(|| app.get_discover_kb_idx() as usize)
            .and_then(|at| basis.keys.get(at))
            .and_then(|key| prepared.frame.keys.iter().position(|next| next == key));
        let mut cards = Vec::with_capacity(prepared.rows.len());
        let mut poster_jobs = Vec::new();
        for (index, row) in prepared.rows.iter_mut().enumerate() {
            let mut card = MediaCard {
                id: row.key.1.clone().into(),
                title: row.title.clone().into(),
                year: row.context.clone().into(),
                badge: row.badge.clone().into(),
                poster_path: row.poster.clone().into(),
                ..Default::default()
            };
            if let Some((at, painted)) = old.remove(&row.key) {
                if painted.poster_path.as_str()
                    != basis
                        .poster_urls
                        .get(at)
                        .map(String::as_str)
                        .unwrap_or_default()
                    && painted.poster_path.as_str() != row.poster
                {
                    // A direct UI delivery changed the URL after preparation.
                    // Preserve that newer memory paint rather than replaying
                    // the older plain row or capturing Image on the worker.
                    row.poster = painted.poster_path.to_string();
                    card.poster_path = painted.poster_path.clone();
                    prepared.metas[index].poster =
                        (!row.poster.is_empty()).then(|| row.poster.clone());
                    Arc::make_mut(&mut Arc::make_mut(&mut prepared.frame).poster_urls)[index] =
                        row.poster.clone();
                    Arc::make_mut(&mut Arc::make_mut(&mut prepared.frame).poster_overrides)
                        .insert(row.key.clone(), row.poster.clone());
                }
                if painted.poster_path == card.poster_path {
                    card.poster = painted.poster;
                    card.is_loaded = painted.is_loaded;
                }
            }
            if !card.is_loaded {
                poster_jobs.push((
                    index,
                    row.key.clone(),
                    row.source.clone(),
                    row.poster.clone(),
                ));
            }
            cards.push(card);
        }
        #[cfg(test)]
        drop(card_sample);
        if !request.stamp.current() {
            self.repaint_search();
            return;
        }
        let Some(model) = model.as_any().downcast_ref::<VecModel<MediaCard>>() else {
            self.repaint_search();
            return;
        };
        let mut cards: Vec<_> = cards.into_iter().map(Some).collect();
        #[cfg(test)]
        let model_sample = search_publication_bench::Phase::begin("model");
        for edit in &prepared.plan {
            match *edit {
                SearchModelEdit::Move(from, to) => {
                    let card = model.remove(from);
                    model.insert(to, card);
                    #[cfg(test)]
                    search_publication_bench::model_edit("move");
                }
                SearchModelEdit::Insert(at) => {
                    model.insert(at, cards[at].as_ref().unwrap().clone());
                    #[cfg(test)]
                    search_publication_bench::model_edit("insert");
                }
                SearchModelEdit::Set(at) => {
                    model.set_row_data(at, cards[at].take().unwrap());
                    #[cfg(test)]
                    search_publication_bench::model_edit("set");
                }
                SearchModelEdit::Remove(at) => {
                    model.remove(at);
                    #[cfg(test)]
                    search_publication_bench::model_edit("remove");
                }
            }
        }
        if let Some(at) = focused {
            app.set_discover_kb_idx(at as i32);
        }
        #[cfg(test)]
        drop(model_sample);
        let model_changed = !basis.keys.is_empty() || !prepared.rows.is_empty();
        let sequence = request.sequence;
        let generation = request.generation;
        let append = request.append;
        let (pending, can_load_more, retired, old_policy, memory_changed) = {
            #[cfg(test)]
            let lock_sample = search_publication_bench::SharedLock::waiting();
            let mut state = self.shared.lock().unwrap();
            #[cfg(test)]
            let _lock_sample = lock_sample.acquired();
            {
                let Shared {
                    entries,
                    search_projection,
                    ..
                } = &mut *state;
                search_projection.capture_entries(entries);
            }
            let memory_changed = state.search_projection.memory_revision != request.memory_revision;
            let retired = std::mem::replace(&mut state.search_previews, prepared.metas);
            state.search_targets = prepared.targets;
            let completed = prepared
                .completed_outcomes
                .saturating_sub(state.search_projection.completed_outcomes)
                as usize;
            state.search_projection.completed_outcomes = prepared.completed_outcomes;
            state.search_pending_requests = state.search_pending_requests.saturating_sub(completed);
            let pending = state.search_pending_requests;
            if pending == 0 {
                state.search_loading_more = false;
            }
            let can_load_more =
                pending == 0 && state.search_targets.iter().any(|target| !target.exhausted);
            let view = &mut state.search_projection;
            view.published = view.published.max(sequence);
            if model_changed {
                view.model_revision = view.model_revision.wrapping_add(1);
            }
            view.frame = prepared.frame;
            view.base_entries = prepared.entries;
            if !memory_changed {
                view.entry_deltas = Arc::default();
            }
            let old_policy = std::mem::replace(&mut view.policy, prepared.policy);
            view.policy_requires_saved = prepared.policy_requires_saved;
            view.local_sort_overrides.clear();
            view.policy_stamp = Some(request.stamp.clone());
            view.last_sources = request.sources.clone();
            view.published_scope = Some(request.local.scope);
            (pending, can_load_more, retired, old_policy, memory_changed)
        };
        drop(retired);
        drop(old_policy);
        if memory_changed {
            // Background admission can alter plain entries while Slint applies
            // edits. Retain that delta and overlay it before this callback ends.
            self.repaint_search_memory();
            return;
        }
        #[cfg(test)]
        search_publication_bench::prepared_stats(
            request.ingress.load(Ordering::Acquire),
            &request.ingress_details.lock().unwrap(),
            &prepared.stats,
        );
        #[cfg(test)]
        let poster_sample = search_publication_bench::Phase::begin("posters");
        for (index, key, source, url) in poster_jobs {
            self.fetch_prepared_search_poster(generation, index, key, source, url);
        }
        #[cfg(test)]
        drop(poster_sample);
        if !request.stamp.current() || profile_policy::entry_blocked() {
            self.retract_search_projection();
            self.repaint_search();
            return;
        }
        let catalogs_visible = app.get_search_scope() <= 1;
        app.set_search_loading(pending > 0 && !append && catalogs_visible);
        app.set_search_loading_more(pending > 0 && append && catalogs_visible);
        app.set_search_can_load_more(can_load_more && catalogs_visible);
        if let Some(history) = prepared.history {
            self.apply_search_history(&history);
        }
        #[cfg(test)]
        search_publication_bench::publication_complete(generation, sequence);
    }

    pub(super) fn repaint_search_memory(&self) {
        let Some(app) = self.app() else { return };
        let scoped = profiles::ProfileStore::capture().is_scoped();
        let (frame, deltas, policy, policy_valid, requires_saved, scope, query) = {
            let mut state = self.shared.lock().unwrap();
            let stamp = state
                .unified_search
                .presentation_stamp(self.downloads.clone());
            let scope = state.unified_search.projection_local().scope;
            let query = state.search.clone();
            let state_sources: Vec<_> = state
                .installed
                .iter()
                .map(|source| SearchSourceReceipt {
                    url: source.url.clone(),
                    generation: source.generation,
                    enabled: source.enabled,
                    available: source.available,
                })
                .collect();
            let Shared {
                entries,
                search_projection: view,
                ..
            } = &mut *state;
            view.capture_entries(entries);
            let source_current = view.last_sources.is_empty() || view.last_sources == state_sources;
            let valid = !profile_policy::entry_blocked()
                && source_current
                && (!scoped
                    || view.policy.is_some()
                        && view.policy_stamp.as_ref().is_some_and(|prior| {
                            prior.instance() == stamp.instance()
                                && prior.visibility_key() == stamp.visibility_key()
                        }));
            (
                view.frame.clone(),
                view.entry_deltas.clone(),
                view.policy.clone(),
                valid,
                view.policy_requires_saved,
                scope,
                query,
            )
        };
        if !policy_valid {
            self.retract_search_projection();
            self.repaint_search();
            return;
        }
        // Clone only the plain key/URL plan basis outside Shared; full remote
        // metadata remains immutable. Each admitted fact changes sparse rows.
        let focused_key = (app.get_kb_active() && app.get_discover_kb_zone() == 3)
            .then(|| app.get_discover_kb_idx() as usize)
            .and_then(|at| frame.keys.get(at).cloned());
        let mut keys = (*frame.keys).clone();
        let mut urls = (*frame.poster_urls).clone();
        let mut changes = Vec::new();
        for ((kind, id), fact) in deltas.iter() {
            let allowed = !scoped
                || policy
                    .as_ref()
                    .is_some_and(|policy| policy.allows(kind, id));
            let title = fact.as_ref().filter(|_| allowed).and_then(|fact| {
                unified_search::memory_saved_document(
                    &query,
                    kind,
                    id,
                    &fact.name,
                    &fact.year,
                    &fact.poster,
                )
            });
            let key = (kind.clone(), id.clone(), String::new());
            let restore = frame
                .remote_rank
                .get(&key)
                .map(|&at| (frame.remote[at].clone(), frame.remote_rows[at].clone(), at));
            changes.push((kind.clone(), id.clone(), key, fact.clone(), title, restore));
        }
        let mut edits = Vec::new();
        let mut retired = Vec::new();
        {
            let mut state = self.shared.lock().unwrap();
            if state.search_previews.len() != keys.len() {
                drop(state);
                self.repaint_search();
                return;
            }
            for (kind, id, key, fact, title, restore) in changes {
                let global = unified_search::global_id(&id);
                let denied = requires_saved && fact.is_none();
                if denied {
                    for at in (0..state.search_previews.len()).rev() {
                        let meta = &state.search_previews[at];
                        let parent = meta
                            .extra
                            .get("novaSearchTitle")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or(&meta.id);
                        if meta.type_ == kind && parent == id {
                            retired.push(state.search_previews.remove(at));
                            keys.remove(at);
                            urls.remove(at);
                            edits.push(SearchMemoryEdit::Remove(at));
                        }
                    }
                    continue;
                }
                if scope <= 1 && global {
                    for at in 0..state.search_previews.len() {
                        let meta = &mut state.search_previews[at];
                        if meta.type_ == kind
                            && meta.id == id
                            && !meta.extra.contains_key("novaLocalSearchAction")
                        {
                            if fact.is_some() {
                                meta.extra.insert("novaSearchLabel".into(), "Saved".into());
                            } else {
                                meta.extra.remove("novaSearchLabel");
                            }
                            let (badge, context) = unified_search::card_text(meta);
                            let force = fact.as_ref().is_some_and(|fact| {
                                fact.poster_admitted && !fact.poster.is_empty()
                            });
                            let poster = if force {
                                fact.as_ref().unwrap().poster.clone()
                            } else {
                                urls[at].clone()
                            };
                            let row = PreparedSearchRow {
                                key: keys[at].clone(),
                                source: meta
                                    .extra
                                    .get("novaSourceUrl")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or_default()
                                    .into(),
                                title: meta.title(),
                                context,
                                badge,
                                poster,
                            };
                            urls[at] = row.poster.clone();
                            edits.push(SearchMemoryEdit::Set(at, row, force));
                        }
                    }
                }
                let at = keys.iter().position(|old| old == &key).filter(|at| {
                    state.search_previews[*at]
                        .extra
                        .get("novaLocalSearchAction")
                        .and_then(serde_json::Value::as_str)
                        == Some("Title")
                });
                if matches!(scope, 0 | 2) {
                    if let Some(meta) = title {
                        let force = fact
                            .as_ref()
                            .is_some_and(|fact| fact.poster_admitted && !fact.poster.is_empty());
                        let (badge, context) = unified_search::card_text(&meta);
                        let poster = if force {
                            fact.as_ref().unwrap().poster.clone()
                        } else {
                            at.or_else(|| {
                                global
                                    .then(|| keys.iter().position(|old| old == &key))
                                    .flatten()
                            })
                            .and_then(|index| urls.get(index).cloned())
                            .or_else(|| restore.as_ref().map(|(_, row, _)| row.poster.clone()))
                            .unwrap_or_else(|| meta.poster.clone().unwrap_or_default())
                        };
                        let row = PreparedSearchRow {
                            key: key.clone(),
                            source: String::new(),
                            title: meta.title(),
                            context,
                            badge,
                            poster,
                        };
                        let mut old_at = at.or_else(|| {
                            global
                                .then(|| keys.iter().position(|old| old == &key))
                                .flatten()
                        });
                        let sort = row.title.to_lowercase();
                        let to = keys
                            .iter()
                            .enumerate()
                            .filter(|(index, _)| Some(*index) != old_at)
                            .position(|(index, old)| {
                                if !state.search_previews[index]
                                    .extra
                                    .contains_key("novaLocalSearchAction")
                                {
                                    return true;
                                }
                                let old_sort = state
                                    .search_projection
                                    .local_sort_overrides
                                    .get(old)
                                    .or_else(|| frame.local_sort.get(old));
                                old_sort.is_some_and(|name| {
                                    (name.as_str(), old.1.as_str(), old.0.as_str())
                                        > (sort.as_str(), key.1.as_str(), key.0.as_str())
                                })
                            })
                            .unwrap_or(keys.len() - usize::from(old_at.is_some()));
                        let local_count = state
                            .search_previews
                            .iter()
                            .filter(|meta| meta.extra.contains_key("novaLocalSearchAction"))
                            .count();
                        let was_local = old_at.is_some_and(|at| {
                            state.search_previews[at]
                                .extra
                                .contains_key("novaLocalSearchAction")
                        });
                        if !was_local && local_count >= 150 {
                            if to >= 150 {
                                continue;
                            }
                            let evicted = state
                                .search_previews
                                .iter()
                                .rposition(|meta| meta.extra.contains_key("novaLocalSearchAction"))
                                .unwrap();
                            let evicted_key = keys.remove(evicted);
                            urls.remove(evicted);
                            retired.push(state.search_previews.remove(evicted));
                            edits.push(SearchMemoryEdit::Remove(evicted));
                            if let Some(index) = old_at.as_mut()
                                && *index > evicted
                            {
                                *index -= 1;
                            }
                            // An evicted portable saved title may still exist
                            // as a catalog hit beyond the bounded local prefix.
                            if scope == 0
                                && unified_search::global_id(&evicted_key.1)
                                && let Some(&rank) = frame.remote_rank.get(&evicted_key)
                            {
                                let meta = frame.remote[rank].clone();
                                let row = frame.remote_rows[rank].clone();
                                let restore_at = keys
                                    .iter()
                                    .enumerate()
                                    .position(|(index, key)| {
                                        !state.search_previews[index]
                                            .extra
                                            .contains_key("novaLocalSearchAction")
                                            && frame
                                                .remote_rank
                                                .get(key)
                                                .is_some_and(|other| *other > rank)
                                    })
                                    .unwrap_or(keys.len());
                                state.search_previews.insert(restore_at, meta);
                                keys.insert(restore_at, evicted_key);
                                urls.insert(restore_at, row.poster.clone());
                                edits.push(SearchMemoryEdit::Insert(restore_at, row));
                                if let Some(index) = old_at.as_mut()
                                    && *index >= restore_at
                                {
                                    *index += 1;
                                }
                            }
                        }
                        if let Some(old_at) = old_at {
                            retired
                                .push(std::mem::replace(&mut state.search_previews[old_at], meta));
                            urls[old_at] = row.poster.clone();
                            edits.push(SearchMemoryEdit::Set(old_at, row.clone(), force));
                            if old_at != to {
                                let meta = state.search_previews.remove(old_at);
                                state.search_previews.insert(to, meta);
                                let k = keys.remove(old_at);
                                keys.insert(to, k);
                                let url = urls.remove(old_at);
                                urls.insert(to, url);
                                edits.push(SearchMemoryEdit::Move(old_at, to));
                            }
                        } else {
                            state.search_previews.insert(to, meta);
                            keys.insert(to, key.clone());
                            urls.insert(to, row.poster.clone());
                            edits.push(SearchMemoryEdit::Insert(to, row));
                        }
                        state
                            .search_projection
                            .local_sort_overrides
                            .insert(key, sort);
                    } else if let Some(at) = at {
                        if scope == 0 && global {
                            if let Some((meta, row, rank)) = restore {
                                retired
                                    .push(std::mem::replace(&mut state.search_previews[at], meta));
                                urls[at] = row.poster.clone();
                                edits.push(SearchMemoryEdit::Set(at, row, false));
                                let to = keys
                                    .iter()
                                    .enumerate()
                                    .filter(|(index, _)| *index != at)
                                    .position(|(index, key)| {
                                        !state.search_previews[index]
                                            .extra
                                            .contains_key("novaLocalSearchAction")
                                            && frame
                                                .remote_rank
                                                .get(key)
                                                .is_some_and(|other| *other > rank)
                                    })
                                    .unwrap_or(keys.len() - 1);
                                if at != to {
                                    let meta = state.search_previews.remove(at);
                                    state.search_previews.insert(to, meta);
                                    let key = keys.remove(at);
                                    keys.insert(to, key);
                                    let url = urls.remove(at);
                                    urls.insert(to, url);
                                    edits.push(SearchMemoryEdit::Move(at, to));
                                }
                            } else {
                                retired.push(state.search_previews.remove(at));
                                keys.remove(at);
                                urls.remove(at);
                                edits.push(SearchMemoryEdit::Remove(at));
                            }
                        } else {
                            retired.push(state.search_previews.remove(at));
                            keys.remove(at);
                            urls.remove(at);
                            edits.push(SearchMemoryEdit::Remove(at));
                        }
                    }
                }
            }
            let view = &mut state.search_projection;
            view.model_revision = view.model_revision.wrapping_add(1);
            // The sparse overlay has already adopted this scope. Its ensuing
            // worker request must not retract those accepted rows as old scope.
            view.published_scope = Some(scope);
            view.frame = Arc::new(SearchPlainFrame {
                keys: Arc::new(keys),
                poster_urls: Arc::new(urls),
                poster_overrides: frame.poster_overrides.clone(),
                poster_stamp: frame.poster_stamp,
                remote: frame.remote.clone(),
                remote_rows: frame.remote_rows.clone(),
                remote_rank: frame.remote_rank.clone(),
                local_sort: frame.local_sort.clone(),
            });
        }
        let model = app.get_search_results();
        if let Some(model) = model.as_any().downcast_ref::<VecModel<MediaCard>>() {
            for edit in edits {
                match edit {
                    SearchMemoryEdit::Remove(at) => {
                        model.remove(at);
                    }
                    SearchMemoryEdit::Move(from, to) => {
                        let card = model.remove(from);
                        model.insert(to, card);
                    }
                    SearchMemoryEdit::Insert(at, row) => model.insert(at, search_plain_card(&row)),
                    SearchMemoryEdit::Set(at, mut row, force) => {
                        let mut card = search_plain_card(&row);
                        if let Some(current) = model.row_data(at) {
                            if !force
                                && current.poster_path.as_str()
                                    != frame
                                        .poster_urls
                                        .get(at)
                                        .map(String::as_str)
                                        .unwrap_or_default()
                            {
                                row.poster = current.poster_path.to_string();
                                card.poster_path = current.poster_path.clone();
                            }
                            if current.poster_path == card.poster_path {
                                card.poster = current.poster;
                                card.is_loaded = current.is_loaded;
                            }
                        }
                        model.set_row_data(at, card);
                    }
                }
            }
        }
        if let Some(key) = focused_key {
            let keys = self
                .shared
                .lock()
                .unwrap()
                .search_projection
                .frame
                .keys
                .clone();
            if let Some(at) = keys.iter().position(|current| current == &key) {
                app.set_discover_kb_idx(at as i32);
            }
        }
        drop(retired);
        self.repaint_search();
    }

    fn retract_search_projection(&self) {
        let old = {
            let mut state = self.shared.lock().unwrap();
            state.search_projection.model_revision =
                state.search_projection.model_revision.wrapping_add(1);
            let frame = state.search_projection.frame.clone();
            state.search_projection.frame = Arc::new(SearchPlainFrame {
                keys: Arc::default(),
                poster_urls: Arc::default(),
                poster_overrides: frame.poster_overrides.clone(),
                poster_stamp: frame.poster_stamp,
                remote: frame.remote.clone(),
                remote_rows: frame.remote_rows.clone(),
                remote_rank: frame.remote_rank.clone(),
                local_sort: frame.local_sort.clone(),
            });
            std::mem::take(&mut state.search_previews)
        };
        if let Some(app) = self.app() {
            let model = app.get_search_results();
            if let Some(model) = model.as_any().downcast_ref::<VecModel<MediaCard>>() {
                while model.row_count() > 0 {
                    model.remove(model.row_count() - 1);
                }
            }
        }
        drop(old);
    }

    #[cfg(test)]
    fn apply_search_results_legacy(
        &self,
        generation: u64,
        pages: Vec<(usize, Option<Vec<MetaPreview>>)>,
        append: bool,
    ) {
        #[cfg(test)]
        let _publication_sample = search_publication_bench::Callback::begin();
        let Some(app) = self.app() else { return };
        let context = profiles::capture();
        let version = progress_projection::revision();
        #[cfg(test)]
        let policy_sample = search_publication_bench::Phase::begin("policy");
        let policy =
            profile_policy::TitlePolicySnapshot::capture_browse(&profiles::ProfileStore::capture());
        #[cfg(test)]
        drop(policy_sample);
        let delivered = !pages.is_empty();
        let (old_previews, ranked_metas, new_count, can_load_more, pending) = {
            #[cfg(test)]
            let lock_sample = search_publication_bench::SharedLock::waiting();
            let mut state = self.shared.lock().unwrap();
            #[cfg(test)]
            let _lock_sample = lock_sample.acquired();
            if generation != state.search_generation {
                return;
            }
            let old_previews = state.search_previews.clone();

            let deliveries = pages.len();
            for (index, metas) in pages {
                record_search_page_outcome(&mut state.search_targets, index, metas);
            }

            let query = state.search.clone();
            let mut merged = ranked_search_results(&query, &state.search_targets);
            merged.retain(|m| {
                policy
                    .as_ref()
                    .is_ok_and(|policy| policy.allows(&m.type_, &m.id))
            });
            if !profiles::is_current(&context) || progress_projection::revision() != version {
                return;
            }
            let merged = unified_search::merge(&state, merged);
            let old_keys: HashSet<_> = old_previews
                .iter()
                .map(unified_search::result_key)
                .collect();
            let new_count = merged
                .iter()
                .filter(|meta| !old_keys.contains(&unified_search::result_key(meta)))
                .count();
            state.search_previews = merged;
            let ranked_metas = state.search_previews.clone();
            state.search_pending_requests =
                state.search_pending_requests.saturating_sub(deliveries);
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
        #[cfg(test)]
        let card_sample = search_publication_bench::Phase::begin("cards");
        let old_model = app.get_search_results();
        let mut old_cards = HashMap::new();
        for (index, meta) in old_previews.iter().enumerate() {
            if let Some(card) = old_model.row_data(index) {
                old_cards.insert(unified_search::result_key(meta), card);
            }
        }

        let mut cards = Vec::with_capacity(ranked_metas.len());
        let mut poster_jobs = Vec::new();
        for meta in &ranked_metas {
            let key = unified_search::result_key(meta);
            let mut known = meta.clone();
            let header = read_meta_header_for(&meta.type_, &meta.id);
            #[cfg(test)]
            search_publication_bench::header_read(header.is_some());
            {
                #[cfg(test)]
                let lock_sample = search_publication_bench::SharedLock::waiting();
                let state = self.shared.lock().unwrap();
                #[cfg(test)]
                let _lock_sample = lock_sample.acquired();
                reuse_discover_poster(&mut known, header.as_ref(), &state.entries);
            }
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
        let focused_index = (app.get_kb_active() && app.get_discover_kb_zone() == 3)
            .then(|| app.get_discover_kb_idx() as usize)
            .and_then(|index| unified_search::restored_focus(&old_previews, &ranked_metas, index));
        #[cfg(test)]
        drop(card_sample);
        #[cfg(test)]
        let model_sample = search_publication_bench::Phase::begin("model");
        update_search_result_model(&app, &old_previews, &ranked_metas, cards);
        if let Some(index) = focused_index {
            app.set_discover_kb_idx(index as i32);
        }
        #[cfg(test)]
        drop(model_sample);
        #[cfg(test)]
        let poster_sample = search_publication_bench::Phase::begin("posters");
        for (type_, id, poster) in poster_jobs {
            self.fetch_search_card_poster(type_, id, poster, generation);
        }
        #[cfg(test)]
        drop(poster_sample);
        let catalogs_visible = app.get_search_scope() <= 1;
        app.set_search_loading(pending > 0 && !append && catalogs_visible);
        app.set_search_loading_more(pending > 0 && append && catalogs_visible);
        app.set_search_can_load_more(can_load_more && catalogs_visible);
        if !append && pending == 0 && delivered {
            let query = {
                #[cfg(test)]
                let lock_sample = search_publication_bench::SharedLock::waiting();
                let state = self.shared.lock().unwrap();
                #[cfg(test)]
                let _lock_sample = lock_sample.acquired();
                state.search.clone()
            };
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
            state.unified_search.clear_scope();
            state.search_generation = state.search_generation.wrapping_add(1);
            state.search.clear();
            state.search_filters = SearchFilters::default();
            state.search_targets.clear();
            state.search_poster_inflight.clear();
            state.search_pending_requests = 0;
            state.search_loading_more = false;
        }
        if let Some(app) = self.app() {
            app.set_search_scope(unified_search::DEFAULT_SCOPE);
            app.set_search_text(SharedString::default());
            app.set_search_loading(false);
            app.set_search_loading_more(false);
            app.set_search_can_load_more(false);
            app.set_discover_search_open(false);
            app.set_discover_search_focused(false);
            app.set_discover_search_filters_open(false);
        }
        self.refresh_local_search("");
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
        let (type_idx, catalog_idx, catalog_names, hint, _searchable, has_grid, supports_skip) = {
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
                text::tr("Search your library, episodes and bookmarks.").to_string()
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
            app.set_searchable(true);
            // can_load_more is set after fetch completes; for now only mark
            // it available when the catalog supports skip.
            app.set_can_load_more(supports_skip);
        } else {
            app.set_searchable_hint(SharedString::from(""));
            app.set_searchable(true);
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
        let (hint, _searchable) = {
            let state = self.shared.lock().unwrap();
            let searchable = !build_search_targets(&state.installed).is_empty();
            let hint = if searchable {
                text::tr("Search movies, shows…").to_string()
            } else {
                text::tr("Search your library, episodes and bookmarks.").to_string()
            };
            (hint, searchable)
        };
        app.set_searchable_hint(SharedString::from(hint));
        app.set_searchable(true);
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
        self.refresh_catalog_pagination_ui();
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
                page_fingerprints: HashSet::new(),
                results: Arc::default(),
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
        .flat_map(|target| target.results.iter())
        .filter(|meta| !meta.id.is_empty() && seen.insert(unified_search::result_key(meta)))
        .cloned()
        .collect()
}

#[cfg(test)]
fn update_search_result_model(
    app: &AppWindow,
    old_previews: &[MetaPreview],
    new_previews: &[MetaPreview],
    cards: Vec<MediaCard>,
) {
    let model = app.get_search_results();
    let Some(model) = model.as_any().downcast_ref::<VecModel<MediaCard>>() else {
        #[cfg(test)]
        search_publication_bench::model_edit("replace");
        app.set_search_results(Rc::new(VecModel::from(cards)).into());
        return;
    };
    if model.row_count() != old_previews.len() {
        #[cfg(test)]
        search_publication_bench::model_edit("replace");
        app.set_search_results(Rc::new(VecModel::from(cards)).into());
        return;
    }

    let mut current_keys: Vec<_> = old_previews
        .iter()
        .map(unified_search::result_key)
        .collect();
    for (index, (meta, card)) in new_previews.iter().zip(cards).enumerate() {
        let key = unified_search::result_key(meta);
        if current_keys.get(index) != Some(&key) {
            if let Some(previous_index) = current_keys
                .iter()
                .enumerate()
                .skip(index + 1)
                .find_map(|(at, old_key)| (old_key == &key).then_some(at))
            {
                let previous_card = model.remove(previous_index);
                #[cfg(test)]
                search_publication_bench::model_edit("move");
                model.insert(index, previous_card);
                current_keys.remove(previous_index);
                current_keys.insert(index, key);
            } else {
                #[cfg(test)]
                search_publication_bench::model_edit("insert");
                model.insert(index, card.clone());
                current_keys.insert(index, key);
            }
        }
        model.set_row_data(index, card);
        #[cfg(test)]
        search_publication_bench::model_edit("set");
    }
    while current_keys.len() > new_previews.len() {
        #[cfg(test)]
        search_publication_bench::model_edit("remove");
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

fn record_search_source_page(
    targets: &mut [SearchTarget],
    index: usize,
    mut metas: Vec<MetaPreview>,
) {
    let Some(target) = targets.get_mut(index) else {
        return;
    };
    for meta in &mut metas {
        unified_search::tag_catalog(meta, &target.addon_url);
    }
    let raw_len = metas.len();
    if raw_len == 0 || !target.supports_skip {
        target.exhausted = true;
    } else {
        // The provider cursor counts its raw rows, including duplicates.
        target.next_skip = target.next_skip.saturating_add(raw_len);
    }
    if raw_len == 0 {
        return;
    }
    let mut identities: Vec<_> = metas.iter().map(unified_search::result_key).collect();
    identities.sort();
    identities.dedup();
    let fingerprint = nova_providers::catalog_grouping::projection_fingerprint(&identities);
    if !target.page_fingerprints.insert(fingerprint) {
        target.exhausted = true;
        return;
    }
    let mut seen: HashSet<_> = target
        .results
        .iter()
        .map(unified_search::result_key)
        .collect();
    let previous_len = target.results.len();
    // Keep the first metadata from each source, matching display deduplication.
    Arc::make_mut(&mut target.results).extend(
        metas
            .into_iter()
            .filter(|meta| seen.insert(unified_search::result_key(meta))),
    );
    // A source may cycle through new subsets of earlier pages without repeating
    // any one page verbatim. No new identities means it cannot advance the view.
    if target.results.len() == previous_len {
        target.exhausted = true;
    }
}

#[cfg(test)]
fn ranked_search_results(query: &str, targets: &[SearchTarget]) -> Vec<MetaPreview> {
    let mut results = unique_search_results(targets);
    // Stable sorting preserves declared source and provider result order for
    // equally relevant rows.
    results.sort_by_cached_key(|meta| std::cmp::Reverse(search_relevance_score(query, meta)));
    results
}

#[cfg(test)]
fn search_relevance_score(query: &str, meta: &MetaPreview) -> u32 {
    #[cfg(test)]
    search_publication_bench::score_call();
    let title = meta.title();
    let year = meta.year_str();
    search_match_score(query, &title, year.as_deref())
}

/// Rank exact, prefix, phrase, complete-token and typo-tolerant title matches
/// in descending tiers. Weak partial-token matches remain at the bottom, and
/// all provider results stay visible regardless of score.
#[cfg(test)]
fn search_match_score(query: &str, title: &str, year: Option<&str>) -> u32 {
    let query = normalize_search_text(query);
    let title = normalize_search_text(title);
    let year = year.map(normalize_search_text);
    search_match_score_normalized(&query, &title, year.as_deref())
}
fn search_match_score_normalized(query: &str, title: &str, year: Option<&str>) -> u32 {
    if query.is_empty() || title.is_empty() {
        return 0;
    }

    let mut title_and_year = title.to_owned();
    if let Some(year) = year
        && !year.is_empty()
    {
        title_and_year.push(' ');
        title_and_year.push_str(year);
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
    if title.starts_with(query) || title_and_year.starts_with(query) {
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
            best_similarity = best_similarity.max(normalized_edit_similarity(query, &window));
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
    // OSA distance is symmetric; keep the three row buffers on the shorter side.
    let (left, right) = if left.len() < right.len() {
        (right, left)
    } else {
        (left, right)
    };
    let mut older = vec![0; right.len() + 1];
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for left_index in 1..=left.len() {
        current[0] = left_index;
        for right_index in 1..=right.len() {
            let substitution_cost = usize::from(left[left_index - 1] != right[right_index - 1]);
            current[right_index] = (previous[right_index] + 1)
                .min(current[right_index - 1] + 1)
                .min(previous[right_index - 1] + substitution_cost);
            if left_index > 1
                && right_index > 1
                && left[left_index - 1] == right[right_index - 2]
                && left[left_index - 2] == right[right_index - 1]
            {
                current[right_index] = current[right_index].min(older[right_index - 2] + 1);
            }
        }
        std::mem::swap(&mut older, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
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
    let (label, context) = unified_search::card_text(meta);
    MediaCard {
        id: SharedString::from(&meta.id),
        title: SharedString::from(meta.title()),
        year: context.into(),
        poster_path: SharedString::from(meta.poster.clone().unwrap_or_default()),
        poster: Image::default(),
        is_loaded: false,
        badge: label.into(),
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

enum SearchMemoryEdit {
    Remove(usize),
    Move(usize, usize),
    Insert(usize, PreparedSearchRow),
    Set(usize, PreparedSearchRow, bool),
}
fn search_plain_card(row: &PreparedSearchRow) -> MediaCard {
    MediaCard {
        id: row.key.1.clone().into(),
        title: row.title.clone().into(),
        year: row.context.clone().into(),
        badge: row.badge.clone().into(),
        poster_path: row.poster.clone().into(),
        ..Default::default()
    }
}
type SearchResultKey = (String, String, String);
type SearchEntryKey = (String, String);
#[derive(Clone, PartialEq, Eq)]
struct SearchEntryFact {
    name: String,
    year: String,
    poster: String,
    poster_admitted: bool,
}
#[derive(Clone, Default)]
struct SearchPlainFrame {
    keys: Arc<Vec<SearchResultKey>>,
    poster_urls: Arc<Vec<String>>,
    poster_overrides: Arc<HashMap<SearchResultKey, String>>,
    poster_stamp: Option<(u64, u64)>,
    remote: Arc<Vec<MetaPreview>>,
    remote_rows: Arc<Vec<PreparedSearchRow>>,
    remote_rank: Arc<HashMap<SearchResultKey, usize>>,
    local_sort: Arc<HashMap<SearchResultKey, String>>,
}
#[derive(Clone)]
struct PreparedSearchRow {
    key: SearchResultKey,
    source: String,
    title: String,
    context: String,
    badge: String,
    poster: String,
}
#[derive(Default)]
pub(super) struct SearchProjectionView {
    generation: u64,
    requested: u64,
    published: u64,
    completed_outcomes: u64,
    model_revision: u64,
    memory_revision: u64,
    seen_entries: HashMap<String, HashMap<String, SearchEntryFact>>,
    seen_count: usize,
    seeded_entries: bool,
    base_entries: Arc<HashMap<SearchEntryKey, SearchEntryFact>>,
    entry_deltas: Arc<HashMap<SearchEntryKey, Option<SearchEntryFact>>>,
    frame: Arc<SearchPlainFrame>,
    policy: Option<Arc<profile_policy::TitlePolicySnapshot>>,
    policy_stamp: Option<unified_search::PresentationStamp>,
    policy_requires_saved: bool,
    local_sort_overrides: HashMap<SearchResultKey, String>,
    last_sources: Vec<SearchSourceReceipt>,
    published_scope: Option<i32>,
}
impl SearchProjectionView {
    fn capture_entries(&mut self, entries: &[LibraryEntry]) {
        let matches = |entry: &LibraryEntry| {
            self.seen_entries
                .get(entry.type_.as_str())
                .and_then(|titles| titles.get(entry.id.as_str()))
                .is_some_and(|old| {
                    old.name == entry.name
                        && old.year == entry.year
                        && old.poster == entry.poster_url
                })
        };
        if entries.len() == self.seen_count && entries.iter().all(matches) {
            self.seeded_entries = true;
            return;
        }
        let live: HashSet<_> = entries
            .iter()
            .map(|entry| (entry.type_.as_str(), entry.id.as_str()))
            .collect();
        let removed: Vec<_> = self
            .seen_entries
            .iter()
            .flat_map(|(kind, titles)| titles.keys().map(move |id| (kind, id)))
            .filter(|(kind, id)| !live.contains(&(kind.as_str(), id.as_str())))
            .map(|(kind, id)| (kind.clone(), id.clone()))
            .collect();
        let changes = Arc::make_mut(&mut self.entry_deltas);
        for (kind, id) in removed {
            self.seen_entries.get_mut(&kind).unwrap().remove(&id);
            changes.insert((kind, id), None);
        }
        for entry in entries {
            let titles = self.seen_entries.entry(entry.type_.clone()).or_default();
            if titles.get(entry.id.as_str()).is_some_and(|old| {
                old.name == entry.name && old.year == entry.year && old.poster == entry.poster_url
            }) {
                continue;
            }
            let poster_admitted = self.seeded_entries
                && titles
                    .get(entry.id.as_str())
                    .is_none_or(|old| old.poster != entry.poster_url)
                || changes
                    .get(&(entry.type_.clone(), entry.id.clone()))
                    .and_then(Option::as_ref)
                    .is_some_and(|fact| fact.poster_admitted && fact.poster == entry.poster_url);
            let fact = SearchEntryFact {
                name: entry.name.clone(),
                year: entry.year.clone(),
                poster: entry.poster_url.clone(),
                poster_admitted,
            };
            titles.insert(entry.id.clone(), fact.clone());
            changes.insert((entry.type_.clone(), entry.id.clone()), Some(fact));
        }
        self.seen_count = entries.len();
        self.seeded_entries = true;
        self.memory_revision = self.memory_revision.wrapping_add(1);
    }
}

#[derive(Clone, PartialEq, Eq)]
struct SearchSourceReceipt {
    url: String,
    generation: u64,
    enabled: bool,
    available: bool,
}
#[derive(Clone)]
pub(super) struct SearchProjectionRequest {
    pub(super) context: profiles::Context,
    pub(super) bridge: Bridge,
    generation: u64,
    query: String,
    local: unified_search::ProjectionLocal,
    stamp: unified_search::PresentationStamp,
    targets: Vec<SearchTarget>,
    sources: Vec<SearchSourceReceipt>,
    pages: Vec<(usize, Option<Vec<MetaPreview>>)>,
    discarded_outcomes: usize,
    sequence: u64,
    memory_revision: u64,
    model_revision: u64,
    base_entries: Arc<HashMap<SearchEntryKey, SearchEntryFact>>,
    entry_deltas: Arc<HashMap<SearchEntryKey, Option<SearchEntryFact>>>,
    frame: Arc<SearchPlainFrame>,
    append: bool,
    #[cfg(test)]
    ingress: Arc<AtomicU64>,
    #[cfg(test)]
    ingress_details: Arc<Mutex<SearchIngressDetails>>,
    pending_outcomes: usize,
    applied_outcomes: u64,
    history_entry: SearchHistoryEntry,
}
impl SearchProjectionRequest {
    pub(super) fn query(&self) -> &str {
        &self.query
    }
    pub(super) fn same_activation(&self, context: &profiles::Context, instance: u64) -> bool {
        self.context == *context && self.stamp.instance() == instance
    }
}
struct SearchProjectionOwner {
    context: profiles::Context,
    generation: u64,
    query: String,
    scope: i32,
    stamp: unified_search::PresentationStamp,
    memory_revision: u64,
    sources: Vec<SearchSourceReceipt>,
}
impl SearchProjectionOwner {
    fn from(request: &SearchProjectionRequest) -> Self {
        let mut context = request.context.clone();
        context.session = None;
        Self {
            context,
            generation: request.generation,
            query: request.query.clone(),
            scope: request.local.scope,
            stamp: request.stamp.clone(),
            memory_revision: request.memory_revision,
            sources: request.sources.clone(),
        }
    }
    fn matches(&self, request: &SearchProjectionRequest) -> bool {
        self.context == request.context
            && self.generation == request.generation
            && self.query == request.query
            && self.scope == request.local.scope
            && self.stamp == request.stamp
            && self.memory_revision == request.memory_revision
            && self.sources == request.sources
    }
}
#[derive(Default)]
pub(super) struct SearchProjectionQueue {
    pub(super) latest: Option<SearchProjectionRequest>,
    owner: Option<SearchProjectionOwner>,
    processor: SearchProjectionProcessor,
}
impl SearchProjectionQueue {
    pub(super) fn submit(
        &mut self,
        mut request: SearchProjectionRequest,
    ) -> Option<SearchProjectionRequest> {
        let mut retired = None;
        if let Some(mut prior) = self.latest.take() {
            if prior.context == request.context
                && prior.generation == request.generation
                && prior.query == request.query
                && prior.stamp.instance() == request.stamp.instance()
            {
                request.discarded_outcomes += prior.discarded_outcomes;
                if prior.sources == request.sources {
                    prior.pages.append(&mut request.pages);
                    request.pages = std::mem::take(&mut prior.pages);
                } else {
                    // A page admitted before an authority change settles its
                    // old request, but cannot supply rows to the new authority.
                    request.discarded_outcomes += prior.pages.len();
                }
                retired = Some(prior);
            } else {
                retired = Some(prior);
            }
        }
        self.owner = Some(SearchProjectionOwner::from(&request));
        self.latest = Some(request);
        retired
    }
    pub(super) fn query_current(&self, request: &SearchProjectionRequest) -> bool {
        self.owner.as_ref().is_some_and(|owner| {
            owner.context == request.context
                && owner.generation == request.generation
                && owner.query == request.query
                && owner.stamp.instance() == request.stamp.instance()
        })
    }
    pub(super) fn current(&self, request: &SearchProjectionRequest) -> bool {
        self.owner
            .as_ref()
            .is_some_and(|owner| owner.matches(request))
            && request.stamp.current()
    }
    pub(super) fn take(&mut self) -> Option<(SearchProjectionRequest, SearchProjectionProcessor)> {
        self.latest
            .take()
            .map(|request| (request, std::mem::take(&mut self.processor)))
    }
    pub(super) fn put_processor(&mut self, processor: SearchProjectionProcessor) {
        self.processor = processor;
    }
}
type SearchProjectionIdentity = (String, u64, u64, u64, String);
#[derive(Default)]
pub(super) struct SearchProjectionProcessor {
    owner: Option<SearchProjectionIdentity>,
    targets: Vec<SearchTarget>,
    source_receipts: Vec<SearchSourceReceipt>,
    header_stamp: Option<(u64, u64)>,
    headers: HashMap<SearchEntryKey, Option<String>>,
    header_order: VecDeque<SearchEntryKey>,
    header_bytes: usize,
    scores: HashMap<SearchResultKey, u32>,
    score_stamp: Option<(u64, u64)>,
    normalized_query: String,
    completed_outcomes: u64,
}
#[cfg(test)]
#[derive(Default, Clone)]
struct SearchIngressDetails {
    allocator: search_publication_bench::AllocationSample,
    wait_ns: u64,
    hold_ns: u64,
}
#[cfg(test)]
#[derive(Default, Clone)]
struct SearchPreparationStats {
    elapsed_ns: u64,
    header_reads: u64,
    header_decodes: u64,
    score_calls: u64,
    #[cfg(test)]
    allocator: search_publication_bench::AllocationSample,
}
struct PreparedSearchProjection {
    request: SearchProjectionRequest,
    targets: Vec<SearchTarget>,
    metas: Vec<MetaPreview>,
    rows: Vec<PreparedSearchRow>,
    frame: Arc<SearchPlainFrame>,
    entries: Arc<HashMap<SearchEntryKey, SearchEntryFact>>,
    policy: Option<Arc<profile_policy::TitlePolicySnapshot>>,
    policy_requires_saved: bool,
    plan: Vec<SearchModelEdit>,
    #[cfg(test)]
    stats: SearchPreparationStats,
    completed_outcomes: u64,
    history: Option<Vec<SearchHistoryEntry>>,
}
#[derive(Debug, PartialEq, Eq)]
enum SearchModelEdit {
    Move(usize, usize),
    Insert(usize),
    Set(usize),
    Remove(usize),
}
fn search_edit_plan(old: &[SearchResultKey], new: &[SearchResultKey]) -> Vec<SearchModelEdit> {
    let mut plan = Vec::new();
    if new.starts_with(old) {
        for index in 0..new.len() {
            if index >= old.len() {
                plan.push(SearchModelEdit::Insert(index));
            }
            plan.push(SearchModelEdit::Set(index));
        }
        return plan;
    }
    let positions: HashMap<_, _> = old.iter().enumerate().map(|(at, key)| (key, at)).collect();
    let mut live = vec![0usize; old.len() + 1];
    for (i, value) in live.iter_mut().enumerate().skip(1) {
        *value = i.isolate_lowest_one();
    }
    let mut remaining = old.len();
    for (index, key) in new.iter().enumerate() {
        if let Some(&original) = positions.get(key) {
            let mut at = original + 1;
            let mut rank = 0;
            while at > 0 {
                rank += live[at];
                at -= at.isolate_lowest_one();
            }
            let previous = index + rank - 1;
            if previous != index {
                plan.push(SearchModelEdit::Move(previous, index));
            }
            let mut at = original + 1;
            while at < live.len() {
                live[at] -= 1;
                at += at.isolate_lowest_one();
            }
            remaining -= 1;
        } else {
            plan.push(SearchModelEdit::Insert(index));
        }
        plan.push(SearchModelEdit::Set(index));
    }
    for left in (0..remaining).rev() {
        plan.push(SearchModelEdit::Remove(new.len() + left));
    }
    plan
}
#[cfg(test)]
static SEARCH_PROJECTION_TEST_GATE: Mutex<Option<Arc<dyn Fn() + Send + Sync>>> = Mutex::new(None);

impl SearchProjectionProcessor {
    fn reset_headers(&mut self) {
        self.headers.clear();
        self.header_order.clear();
        self.header_bytes = 0;
        self.header_stamp = None;
    }
    fn select_owner(&mut self, owner: SearchProjectionIdentity) -> bool {
        if self.owner.as_ref() == Some(&owner) {
            return false;
        }
        self.reset_headers();
        self.owner = Some(owner);
        true
    }
    fn replace_sources(
        &mut self,
        sources: &[SearchSourceReceipt],
    ) -> Option<Vec<SearchSourceReceipt>> {
        if self.source_receipts == sources {
            return None;
        }
        self.reset_headers();
        Some(std::mem::replace(
            &mut self.source_receipts,
            sources.to_vec(),
        ))
    }
    fn prepare(
        &mut self,
        request: SearchProjectionRequest,
        current: impl Fn() -> bool,
        query_current: impl Fn() -> bool,
    ) -> Option<PreparedSearchProjection> {
        if !query_current() {
            return None;
        }
        let owner = (
            request.context.id.clone(),
            request.context.epoch,
            request.stamp.instance(),
            request.generation,
            request.query.clone(),
        );
        if self.select_owner(owner) {
            self.targets = request.targets.clone();
            self.source_receipts = request.sources.clone();
            self.scores.clear();
            self.completed_outcomes = 0;
            self.normalized_query = normalize_search_text(&request.query);
        }
        if let Some(old_sources) = self.replace_sources(&request.sources) {
            let old = std::mem::take(&mut self.targets);
            self.targets = request
                .targets
                .iter()
                .map(|target| {
                    let mut next = target.clone();
                    let before = old_sources
                        .iter()
                        .find(|source| source.url == target.addon_url);
                    let after = request
                        .sources
                        .iter()
                        .find(|source| source.url == target.addon_url);
                    let unchanged = before == after
                        && after.is_some_and(|source| source.enabled && source.available);
                    if unchanged {
                        if let Some(prior) = old.iter().find(|prior| {
                            prior.addon_url == target.addon_url
                                && prior.type_ == target.type_
                                && prior.catalog_id == target.catalog_id
                                && prior.genre == target.genre
                        }) {
                            next = prior.clone();
                        }
                    } else {
                        next.results = Arc::default();
                        next.page_fingerprints.clear();
                        next.next_skip = 0;
                    }
                    next
                })
                .collect();
            self.scores.clear();
        }
        self.completed_outcomes = self
            .completed_outcomes
            .saturating_add(request.discarded_outcomes as u64);
        for (index, mut page) in request.pages.iter().cloned() {
            if let Some(target) = self.targets.get(index)
                && request
                    .sources
                    .iter()
                    .find(|source| source.url == target.addon_url)
                    .is_some_and(|source| !source.enabled || !source.available)
            {
                page = None;
            }
            record_search_page_outcome(&mut self.targets, index, page);
            self.completed_outcomes = self.completed_outcomes.wrapping_add(1);
        }
        #[cfg(test)]
        {
            let gate = SEARCH_PROJECTION_TEST_GATE.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate();
            }
        }
        if !current() {
            return None;
        }
        let mut entries = (*request.base_entries).clone();
        for (key, value) in request.entry_deltas.iter() {
            if let Some(value) = value {
                entries.insert(key.clone(), value.clone());
            } else {
                entries.remove(key);
            }
        }
        let entries = Arc::new(entries);
        let saved: HashSet<_> = entries.keys().cloned().collect();
        let policy =
            profile_policy::TitlePolicySnapshot::capture_browse(&profiles::ProfileStore::capture())
                .ok()
                .map(Arc::new);
        if !current() {
            return None;
        }
        #[cfg(test)]
        let mut stats = SearchPreparationStats::default();
        if self.score_stamp != Some(request.stamp.metadata_key()) {
            self.scores.clear();
            self.score_stamp = Some(request.stamp.metadata_key());
        }
        let mut remote = unique_search_results(&self.targets);
        remote.sort_by_cached_key(|meta| {
            let key = unified_search::result_key(meta);
            let score = *self.scores.entry(key).or_insert_with(|| {
                #[cfg(test)]
                {
                    stats.score_calls += 1;
                }
                {
                    let title = normalize_search_text(&meta.title());
                    let year = meta.year_str().map(|year| normalize_search_text(&year));
                    search_match_score_normalized(&self.normalized_query, &title, year.as_deref())
                }
            });
            std::cmp::Reverse(score)
        });
        let requires_saved = profile_policy::restricted() && !profile_policy::management_browsing();
        let allowed = |kind: &str, id: &str| {
            policy
                .as_ref()
                .is_some_and(|policy| policy.allows(kind, id))
                && (!requires_saved || entries.contains_key(&(kind.to_owned(), id.to_owned())))
        };
        remote.retain(|meta| allowed(&meta.type_, &meta.id));
        let local = unified_search::replace_saved_documents(
            &request.local,
            entries
                .iter()
                .filter(|((kind, id), _)| allowed(kind, id))
                .filter_map(|((kind, id), fact)| {
                    unified_search::memory_saved_document(
                        &request.query,
                        kind,
                        id,
                        &fact.name,
                        &fact.year,
                        &fact.poster,
                    )
                }),
            &saved,
        );
        let metas = unified_search::merge_plain(&local, &saved, remote.clone());
        if self.header_stamp != Some(request.stamp.metadata_key()) {
            self.headers.clear();
            self.header_order.clear();
            self.header_bytes = 0;
            self.header_stamp = Some(request.stamp.metadata_key());
        }
        let mut hydrate = |meta: &MetaPreview| -> PreparedSearchRow {
            let key = (meta.type_.clone(), meta.id.clone());
            let poster = self.headers.get(&key).cloned().unwrap_or_else(|| {
                #[cfg(test)]
                {
                    stats.header_reads += 1;
                }
                let header = read_meta_header_for(&meta.type_, &meta.id);
                #[cfg(test)]
                {
                    stats.header_decodes += u64::from(header.is_some());
                }
                let poster = header.and_then(|header| {
                    (!header.poster_url.trim().is_empty()).then_some(header.poster_url)
                });
                let bytes = key.0.len() + key.1.len() + poster.as_ref().map_or(0, String::len);
                const MAX_BYTES: usize = 4 * 1024 * 1024;
                const MAX_HEADERS: usize = 16_384;
                while self.headers.len() >= MAX_HEADERS
                    || self.header_bytes.saturating_add(bytes) > MAX_BYTES
                {
                    let Some(old) = self.header_order.pop_front() else {
                        break;
                    };
                    if let Some(value) = self.headers.remove(&old) {
                        self.header_bytes = self.header_bytes.saturating_sub(
                            old.0.len() + old.1.len() + value.as_ref().map_or(0, String::len),
                        );
                    }
                }
                if bytes <= MAX_BYTES {
                    self.header_bytes += bytes;
                    self.header_order.push_back(key.clone());
                    self.headers.insert(key.clone(), poster.clone());
                }
                poster
            });
            let poster = request
                .entry_deltas
                .get(&key)
                .and_then(Option::as_ref)
                .filter(|fact| fact.poster_admitted && !fact.poster.trim().is_empty())
                .map(|fact| fact.poster.clone())
                .or_else(|| {
                    (request.frame.poster_stamp == Some(request.stamp.metadata_key()))
                        .then(|| {
                            request
                                .frame
                                .poster_overrides
                                .get(&unified_search::result_key(meta))
                        })
                        .flatten()
                        .cloned()
                })
                .or(poster)
                .or_else(|| {
                    entries.get(&key).and_then(|entry| {
                        (!entry.poster.trim().is_empty()).then(|| entry.poster.clone())
                    })
                })
                .or_else(|| meta.poster.clone())
                .unwrap_or_default();
            let (badge, context) = unified_search::card_text(meta);
            PreparedSearchRow {
                key: unified_search::result_key(meta),
                source: meta
                    .extra
                    .get("novaSourceUrl")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                title: meta.title(),
                context,
                badge,
                poster,
            }
        };
        let rows: Vec<_> = metas.iter().map(&mut hydrate).collect();
        let remote_rows = remote.iter().map(&mut hydrate).collect();
        let frame = Arc::new(SearchPlainFrame {
            keys: Arc::new(rows.iter().map(|row| row.key.clone()).collect()),
            poster_urls: Arc::new(rows.iter().map(|row| row.poster.clone()).collect()),
            poster_stamp: Some(request.stamp.metadata_key()),
            poster_overrides: Arc::new(
                rows.iter()
                    .filter_map(|row| {
                        let typed = (row.key.0.clone(), row.key.1.clone());
                        let admitted = request
                            .entry_deltas
                            .get(&typed)
                            .and_then(Option::as_ref)
                            .is_some_and(|fact| fact.poster_admitted && !fact.poster.is_empty());
                        let retained = request.frame.poster_stamp
                            == Some(request.stamp.metadata_key())
                            && request.frame.poster_overrides.contains_key(&row.key);
                        (admitted || retained).then(|| (row.key.clone(), row.poster.clone()))
                    })
                    .collect(),
            ),
            remote_rank: Arc::new(
                remote
                    .iter()
                    .enumerate()
                    .map(|(at, meta)| (unified_search::result_key(meta), at))
                    .collect(),
            ),
            local_sort: Arc::new(
                metas
                    .iter()
                    .filter(|meta| meta.extra.contains_key("novaLocalSearchAction"))
                    .map(|meta| (unified_search::result_key(meta), meta.name.to_lowercase()))
                    .collect(),
            ),
            remote: Arc::new(remote),
            remote_rows: Arc::new(remote_rows),
        });
        let plan = search_edit_plan(&request.frame.keys, &frame.keys);
        if !current() {
            return None;
        }
        let newly_completed = self
            .completed_outcomes
            .saturating_sub(request.applied_outcomes) as usize;
        let history = if newly_completed > 0
            && request.pending_outcomes.saturating_sub(newly_completed) == 0
            && !request.append
        {
            let mut history = read_search_history();
            remember_search_entry(&mut history, request.history_entry.clone());
            if current() {
                write_json("discover:search_history:v2", &history);
                Some(history)
            } else {
                None
            }
        } else {
            None
        };
        Some(PreparedSearchProjection {
            request,
            targets: self.targets.clone(),
            metas,
            rows,
            frame,
            entries,
            policy,
            policy_requires_saved: requires_saved,
            plan,
            #[cfg(test)]
            stats,
            completed_outcomes: self.completed_outcomes,
            history,
        })
    }
}

#[cfg(test)]
mod search_publication_bench {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    static ENABLED: AtomicBool = AtomicBool::new(false);
    static COUNTING: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        static ALLOCATION_SCOPE: Cell<bool> = const { Cell::new(false) };
        static ALLOCATIONS: Cell<AllocationSample> = const { Cell::new(AllocationSample::empty()) };
        static CURRENT: RefCell<Option<Active>> = const { RefCell::new(None) };
        static SAMPLES: RefCell<Vec<Sample>> = const { RefCell::new(Vec::new()) };
    }

    #[derive(Clone, Copy, Default, serde::Serialize)]
    pub(super) struct AllocationSample {
        allocations: u64,
        reallocations: u64,
        deallocations: u64,
        requested_bytes: u64,
    }
    impl AllocationSample {
        const fn empty() -> Self {
            Self {
                allocations: 0,
                reallocations: 0,
                deallocations: 0,
                requested_bytes: 0,
            }
        }
    }

    // The library test executable otherwise uses System. This test-only wrapper
    // delegates unchanged; counting is scoped to the callback's UI thread, and
    // no benchmark TLS/counters are touched during ordinary test allocations.
    struct MeasuredSystem;
    #[global_allocator]
    static ALLOCATOR: MeasuredSystem = MeasuredSystem;
    fn allocation(kind: u8, size: usize) {
        if COUNTING.load(Ordering::Relaxed) == 0 {
            return;
        }
        let _ = ALLOCATION_SCOPE.try_with(|scope| {
            if scope.get() {
                let _ = ALLOCATIONS.try_with(|value| {
                    let mut sample = value.get();
                    match kind {
                        0 => sample.allocations += 1,
                        1 => sample.reallocations += 1,
                        _ => sample.deallocations += 1,
                    }
                    if kind != 2 {
                        sample.requested_bytes += size as u64;
                    }
                    value.set(sample);
                });
            }
        });
    }
    unsafe impl GlobalAlloc for MeasuredSystem {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc(layout) };
            if !ptr.is_null() {
                allocation(0, layout.size());
            }
            ptr
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc_zeroed(layout) };
            if !ptr.is_null() {
                allocation(0, layout.size());
            }
            ptr
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            let result = unsafe { System.realloc(ptr, layout, size) };
            if !result.is_null() {
                allocation(1, size);
            }
            result
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            allocation(2, 0);
            unsafe { System.dealloc(ptr, layout) };
        }
    }

    #[derive(Clone, Default, serde::Serialize)]
    struct Sample {
        callback_ns: u64,
        ingress_ns: u64,
        ingress_shared_wait_ns: u64,
        ingress_shared_hold_ns: u64,
        ingress_allocator: AllocationSample,
        worker_preparation_ns: u64,
        worker_header_reads: u64,
        worker_header_decodes: u64,
        worker_score_calls: u64,
        worker_allocator: AllocationSample,
        shared_wait_ns: u64,
        shared_hold_ns: u64,
        shared_acquisitions: u64,
        policy_snapshot_ns: u64,
        card_preparation_ns: u64,
        model_diff_ns: u64,
        poster_dispatch_ns: u64,
        poster_slot_validations: u64,
        poster_fallback_scans: u64,
        poster_source_checks: u64,
        header_reads: u64,
        header_decodes: u64,
        score_calls: u64,
        model_replacements: u64,
        model_moves: u64,
        model_inserts: u64,
        model_removes: u64,
        model_sets: u64,
        allocator: AllocationSample,
    }
    struct Active {
        began: Instant,
        sample: Sample,
        recovery_counts: [u64; 3],
    }
    fn ns(value: std::time::Duration) -> u64 {
        u64::try_from(value.as_nanos()).unwrap_or(u64::MAX)
    }
    fn update(body: impl FnOnce(&mut Sample)) {
        if ENABLED.load(Ordering::Relaxed) {
            CURRENT.with(|active| {
                if let Some(active) = active.borrow_mut().as_mut() {
                    body(&mut active.sample);
                }
            });
        }
    }
    pub(super) struct AllocationScope(bool);
    impl AllocationScope {
        pub(super) fn begin() -> Self {
            let active = ENABLED.load(Ordering::Relaxed)
                && ALLOCATION_SCOPE.with(|scope| !scope.replace(true));
            if active {
                ALLOCATIONS.with(|sample| sample.set(AllocationSample::empty()));
                COUNTING.fetch_add(1, Ordering::Relaxed);
            }
            Self(active)
        }
        pub(super) fn finish(mut self) -> AllocationSample {
            let sample = if self.0 {
                ALLOCATIONS.with(Cell::get)
            } else {
                AllocationSample::empty()
            };
            self.stop();
            sample
        }
        fn stop(&mut self) {
            if self.0 {
                ALLOCATION_SCOPE.with(|scope| scope.set(false));
                COUNTING.fetch_sub(1, Ordering::Relaxed);
                self.0 = false;
            }
        }
    }
    impl Drop for AllocationScope {
        fn drop(&mut self) {
            self.stop();
        }
    }
    pub(super) struct Callback(bool);
    impl Callback {
        pub(super) fn begin() -> Self {
            if !ENABLED.load(Ordering::Relaxed) {
                return Self(false);
            }
            let begin = CURRENT.with(|active| {
                if active.borrow().is_some() {
                    return false;
                }
                *active.borrow_mut() = Some(Active {
                    began: Instant::now(),
                    sample: Sample::default(),
                    recovery_counts: posters::search_recovery_counts(),
                });
                true
            });
            if begin {
                ALLOCATIONS.with(|sample| sample.set(AllocationSample::empty()));
                ALLOCATION_SCOPE.with(|scope| scope.set(true));
                COUNTING.fetch_add(1, Ordering::Relaxed);
            }
            Self(begin)
        }
    }
    impl Drop for Callback {
        fn drop(&mut self) {
            if !self.0 {
                return;
            }
            COUNTING.fetch_sub(1, Ordering::Relaxed);
            ALLOCATION_SCOPE.with(|scope| scope.set(false));
            let mut active = CURRENT.with(|active| active.borrow_mut().take().unwrap());
            let recovery = posters::search_recovery_counts();
            active.sample.poster_slot_validations = recovery[0] - active.recovery_counts[0];
            active.sample.poster_fallback_scans = recovery[1] - active.recovery_counts[1];
            active.sample.poster_source_checks = recovery[2] - active.recovery_counts[2];
            active.sample.callback_ns = ns(active.began.elapsed());
            active.sample.allocator = ALLOCATIONS.with(Cell::get);
            SAMPLES.with(|samples| samples.borrow_mut().push(active.sample));
        }
    }
    pub(super) struct SharedLock {
        waiting: Option<Instant>,
        held: Option<Instant>,
    }
    impl SharedLock {
        pub(super) fn waiting() -> Self {
            Self {
                waiting: ENABLED.load(Ordering::Relaxed).then(Instant::now),
                held: None,
            }
        }
        pub(super) fn acquired(mut self) -> Self {
            if let Some(waiting) = self.waiting.take() {
                let now = Instant::now();
                update(|sample| {
                    sample.shared_wait_ns += ns(now.duration_since(waiting));
                    sample.shared_acquisitions += 1;
                });
                self.held = Some(now);
            }
            self
        }
    }
    impl Drop for SharedLock {
        fn drop(&mut self) {
            if let Some(held) = self.held {
                update(|sample| sample.shared_hold_ns += ns(held.elapsed()));
            }
        }
    }
    static WAIT_GENERATION: AtomicU64 = AtomicU64::new(0);
    static WAIT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    pub(super) fn prepared_stats(
        ingress: u64,
        details: &SearchIngressDetails,
        stats: &SearchPreparationStats,
    ) {
        update(|sample| {
            sample.ingress_ns = ingress;
            sample.ingress_shared_wait_ns = details.wait_ns;
            sample.ingress_shared_hold_ns = details.hold_ns;
            sample.ingress_allocator = details.allocator;
            sample.worker_preparation_ns = stats.elapsed_ns;
            sample.worker_header_reads = stats.header_reads;
            sample.worker_header_decodes = stats.header_decodes;
            sample.worker_score_calls = stats.score_calls;
            sample.worker_allocator = stats.allocator;
        });
    }
    pub(super) fn publication_complete(generation: u64, sequence: u64) {
        if ENABLED.load(Ordering::Relaxed)
            && WAIT_GENERATION.load(Ordering::Acquire) == generation
            && sequence >= WAIT_SEQUENCE.load(Ordering::Acquire)
        {
            slint::quit_event_loop().unwrap();
        }
    }
    pub(super) fn header_read(decoded: bool) {
        update(|sample| {
            sample.header_reads += 1;
            sample.header_decodes += u64::from(decoded);
        });
    }
    pub(super) fn score_call() {
        update(|sample| sample.score_calls += 1);
    }
    pub(super) struct Phase {
        name: &'static str,
        began: Option<Instant>,
    }
    impl Phase {
        pub(super) fn begin(name: &'static str) -> Self {
            Self {
                name,
                began: ENABLED.load(Ordering::Relaxed).then(Instant::now),
            }
        }
    }
    impl Drop for Phase {
        fn drop(&mut self) {
            if let Some(began) = self.began {
                let elapsed = ns(began.elapsed());
                update(|sample| match self.name {
                    "policy" => sample.policy_snapshot_ns += elapsed,
                    "cards" => sample.card_preparation_ns += elapsed,
                    "model" => sample.model_diff_ns += elapsed,
                    _ => sample.poster_dispatch_ns += elapsed,
                });
            }
        }
    }
    pub(super) fn model_edit(kind: &str) {
        update(|sample| match kind {
            "replace" => sample.model_replacements += 1,
            "move" => sample.model_moves += 1,
            "insert" => sample.model_inserts += 1,
            "remove" => sample.model_removes += 1,
            _ => sample.model_sets += 1,
        });
    }
    fn take_sample() -> Sample {
        SAMPLES.with(|samples| {
            let mut samples = samples.borrow_mut();
            assert_eq!(
                samples.len(),
                1,
                "one real publication callback per scheduled delivery"
            );
            samples.pop().unwrap()
        })
    }
    fn percentile(samples: &[u64], percent: usize) -> u64 {
        let mut values = samples.to_vec();
        values.sort_unstable();
        values[((values.len() * percent).div_ceil(100)).saturating_sub(1)]
    }
    fn target(index: usize) -> SearchTarget {
        SearchTarget {
            addon_url: format!("https://ui05-{index}.invalid/manifest.json"),
            type_: "movie".into(),
            catalog_id: format!("fixed-{index}"),
            genre_options: Vec::new(),
            genre: String::new(),
            supports_skip: true,
            next_skip: 0,
            exhausted: false,
            page_fingerprints: HashSet::new(),
            results: Arc::default(),
        }
    }
    fn metadata(rows: usize, providers: usize) -> Vec<Vec<MetaPreview>> {
        let mut pages = vec![Vec::new(); providers];
        for row in 0..rows {
            let source = row % providers;
            let mut meta = MetaPreview {
                id: if row % 2 == 0 {
                    format!("tt{:07}", 1_000_000 + row)
                } else {
                    format!("opaque-{}", row / providers)
                },
                type_: if row % 11 == 0 { "series" } else { "movie" }.into(),
                name: match row % 5 {
                    0 => "Signal".into(),
                    1 => format!("Signal Journey {row}"),
                    2 => format!("The Silent Signal {row}"),
                    3 => format!("Sginal Adventure {row}"),
                    _ => format!("Unrelated Story {row}"),
                },
                ..Default::default()
            };
            meta.extra
                .insert("year".into(), serde_json::json!(2020 + row % 7));
            unified_search::tag_catalog(&mut meta, &target(source).addon_url);
            pages[source].push(meta);
        }
        pages
    }
    type ResultKey = (String, String, String);
    type Signature = Vec<(ResultKey, u32, String, String, String, serde_json::Value)>;
    fn signature(metas: &[MetaPreview]) -> Signature {
        metas
            .iter()
            .map(|meta| {
                let (badge, context) = unified_search::card_text(meta);
                (
                    unified_search::result_key(meta),
                    search_relevance_score("Signal", meta),
                    meta.name.clone(),
                    badge,
                    context,
                    meta.extra
                        .get("novaLocalSearchAction")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                )
            })
            .collect()
    }
    // Match Discover's actual button label, including its trusted caption.
    // Title alone can select a different unbadged row with the same name, or
    // find nothing when only badged rows are mounted by the viewport.
    fn card_accessible_label(card: &MediaCard) -> String {
        let mut label = card.title.to_string();
        if !card.badge.is_empty() {
            label.push_str(", ");
            label.push_str(card.badge.as_str());
            if !card.year.is_empty() {
                label.push_str(", ");
                label.push_str(card.year.as_str());
            }
        }
        label
    }

    #[test]
    fn publication_fixture_uses_discover_card_accessible_labels() {
        for (badge, year, expected) in [
            ("", "2026", "Signal"),
            ("Saved", "", "Signal, Saved"),
            ("Saved", "2026", "Signal, Saved, 2026"),
        ] {
            let card = MediaCard {
                title: "Signal".into(),
                badge: badge.into(),
                year: year.into(),
                ..Default::default()
            };
            assert_eq!(card_accessible_label(&card), expected);
        }
    }
    fn deliver(
        bridge: &Bridge,
        generation: u64,
        pages: Vec<(usize, Option<Vec<MetaPreview>>)>,
    ) -> Sample {
        let bridge = bridge.clone();
        WAIT_GENERATION.store(generation, Ordering::Release);
        WAIT_SEQUENCE.store(u64::MAX, Ordering::Release);
        profiles::invoke_account(move || {
            bridge.apply_search_results(generation, pages, false);
            if std::env::var_os("NOVA_UI05_LEGACY").is_some() {
                slint::quit_event_loop().unwrap();
            } else {
                WAIT_SEQUENCE.store(
                    bridge.shared.lock().unwrap().search_projection.requested,
                    Ordering::Release,
                );
            }
        })
        .unwrap();
        ENABLED.store(true, Ordering::Relaxed);
        slint::run_event_loop().unwrap();
        ENABLED.store(false, Ordering::Relaxed);
        WAIT_GENERATION.store(0, Ordering::Release);
        take_sample()
    }

    #[test]
    #[ignore = "explicit native UI05 publication baseline; no timing assertions"]
    fn search_publication_native_baseline() {
        const CHILD: &str = "NOVA_UI05_PUBLICATION_ROOT";
        let Some(root) = std::env::var_os(CHILD) else {
            let root =
                std::env::temp_dir().join(format!("nova-ui05-baseline-{}", std::process::id()));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(CHILD, &root)
                .args([
                    "--exact",
                    "app::catalog::search_publication_bench::search_publication_native_baseline",
                    "--ignored",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            let completed = root.join("completed").is_file();
            let _ = std::fs::remove_dir_all(root);
            assert!(
                output.status.success() && completed,
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            print!("{}", String::from_utf8_lossy(&output.stdout));
            return;
        };
        let root = PathBuf::from(root);
        storage::init_at(&root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = AppWindow::new().unwrap();
        app.set_show_home(false);
        app.set_discover_search_open(true);
        app.set_search_text("Signal".into());
        app.set_discover_search_animate_results(false);
        app.set_discover_scroll_y(137.0);
        app.window().set_size(slint::PhysicalSize::new(1280, 800));
        app.show().unwrap();
        let player = crate::player::Player::setup(&app);
        #[cfg(feature = "desktop")]
        let bridge = Bridge::new(
            app.as_weak(),
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        #[cfg(not(feature = "desktop"))]
        let bridge = Bridge::new(
            app.as_weak(),
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        let repeats = std::env::var("NOVA_UI05_REPEATS")
            .ok()
            .map(|v| v.parse::<usize>().unwrap())
            .unwrap_or(3);
        assert!((1..=100).contains(&repeats));
        let sizes: Vec<usize> = std::env::var("NOVA_UI05_ROWS")
            .unwrap_or_else(|_| "100,1000,10000".into())
            .split(',')
            .map(|v| v.parse().unwrap())
            .collect();
        assert!(!sizes.is_empty() && sizes.iter().all(|n| [100, 1000, 10000].contains(n)));
        let mut exact_output = std::env::var_os("NOVA_UI05_SIGNATURES")
            .map(|path| std::fs::File::create(path).unwrap());
        let mut expected_output = std::env::var_os("NOVA_UI05_EXPECT_SIGNATURES").map(|path| {
            use std::io::BufRead;
            std::io::BufReader::new(std::fs::File::open(path).unwrap())
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(&line.unwrap()).unwrap())
                .collect::<std::collections::VecDeque<_>>()
        });
        let mut generation = 100;
        for providers in [1, 10, 30] {
            for &rows in &sizes {
                let pages = metadata(rows, providers);
                let entries: Vec<LibraryEntry> = pages
                    .iter()
                    .flatten()
                    .filter(|meta| meta.id.starts_with("tt") && meta.id.ends_with('0'))
                    .map(|meta| {
                        serde_json::from_value(
                            serde_json::json!({"id":meta.id,"type_":meta.type_,"name":meta.name}),
                        )
                        .unwrap()
                    })
                    .collect();
                let header = serde_json::to_string(&MetaHeader {
                    description: "fixed benchmark metadata ".repeat(8),
                    year: "2026".into(),
                    ..Default::default()
                })
                .unwrap();
                let headers: Vec<_> = pages
                    .iter()
                    .flatten()
                    .map(|meta| (meta_header_key(&meta.type_, &meta.id), Some(header.clone())))
                    .collect();
                storage::try_write_batch(&headers).unwrap();
                for order_name in ["forward", "reverse", "interleaved"] {
                    let mut order: Vec<_> = (0..providers).collect();
                    if order_name == "reverse" {
                        order.reverse();
                    }
                    if order_name == "interleaved" {
                        order.sort_by_key(|i| (i % 2, std::cmp::Reverse(*i)));
                    }
                    let mut callbacks = Vec::new();
                    let mut waits = Vec::new();
                    let mut holds = Vec::new();
                    let mut first_latencies = Vec::new();
                    let mut last_latencies = Vec::new();
                    for repeat in 0..repeats {
                        generation += 1;
                        let mut oracle_targets: Vec<_> = (0..providers).map(target).collect();
                        {
                            let mut state = bridge.shared.lock().unwrap();
                            state.search = "Signal".into();
                            state.search_generation = generation;
                            state.search_targets = (0..providers).map(target).collect();
                            state.search_previews.clear();
                            state.search_pending_requests = providers;
                            state.entries = entries.clone();
                            // Fixed cached metadata only. No fixture addon may
                            // issue HTTP or retain a pending artwork recovery.
                            state.installed = vec![Installed {
                                available: true,
                                generation: 0,
                                url: "https://ui05-disabled.invalid/manifest.json".into(),
                                label: "UI05 fixed metadata".into(),
                                enabled: false,
                                configure_ok: None,
                                manifest: Manifest {
                                    id: "ui05-disabled".into(),
                                    version: "1".into(),
                                    name: "UI05 fixed metadata".into(),
                                    description: None,
                                    resources: Vec::new(),
                                    types: Vec::new(),
                                    catalogs: Vec::new(),
                                    id_prefixes: Vec::new(),
                                    logo: None,
                                    background: None,
                                    extra: Default::default(),
                                },
                            }];
                            state.pending_artwork_recovery.clear();
                        }
                        app.set_search_results(
                            Rc::new(VecModel::from(Vec::<MediaCard>::new())).into(),
                        );
                        app.set_kb_active(true);
                        app.set_discover_kb_zone(3);
                        app.set_discover_kb_idx(0);
                        app.set_discover_search_animate_results(false);
                        let model_identity = app.get_search_results();
                        let mut anchor: Option<ResultKey> = None;
                        let mut reference_pixel = SharedPixelBuffer::<Rgba8Pixel>::new(1, 1);
                        reference_pixel
                            .make_mut_bytes()
                            .copy_from_slice(&[17, 31, 47, 255]);
                        let mut first = None;
                        let stream_started = Instant::now();
                        let mut validation_time = std::time::Duration::ZERO;
                        for (stage, &source) in order.iter().enumerate() {
                            let validation_started = Instant::now();
                            record_search_page_outcome(
                                &mut oracle_targets,
                                source,
                                Some(pages[source].clone()),
                            );
                            let oracle = {
                                let mut state = bridge.shared.lock().unwrap();
                                let actual_targets = std::mem::replace(
                                    &mut state.search_targets,
                                    oracle_targets.clone(),
                                );
                                let expected = unified_search::merge(
                                    &state,
                                    ranked_search_results("Signal", &state.search_targets),
                                );
                                oracle_targets =
                                    std::mem::replace(&mut state.search_targets, actual_targets);
                                expected
                            };
                            let expected_signature = signature(&oracle);
                            validation_time += validation_started.elapsed();
                            let sample = deliver(
                                &bridge,
                                generation,
                                vec![(source, Some(pages[source].clone()))],
                            );
                            let delivered_at =
                                ns(stream_started.elapsed().saturating_sub(validation_time));
                            first.get_or_insert(delivered_at);
                            let validation_started = Instant::now();
                            let actual = bridge.shared.lock().unwrap().search_previews.clone();
                            assert_eq!(
                                signature(&actual),
                                expected_signature,
                                "stage ordering/score/action mismatch"
                            );
                            assert!(
                                !actual.is_empty(),
                                "each completed provider can publish without a fan-out barrier"
                            );
                            if stage + 1 == order.len() {
                                assert_eq!(actual.len(), rows);
                            }
                            assert!(
                                bridge
                                    .shared
                                    .lock()
                                    .unwrap()
                                    .pending_artwork_recovery
                                    .is_empty()
                            );
                            if std::env::var_os("NOVA_UI05_LEGACY").is_some() {
                                assert_eq!(sample.header_reads, actual.len() as u64);
                                assert_eq!(sample.header_decodes, actual.len() as u64);
                                assert_eq!(sample.score_calls, actual.len() as u64);
                            } else {
                                assert_eq!(
                                    sample.header_reads, 0,
                                    "publication cannot read metadata cache"
                                );
                                assert_eq!(sample.header_decodes, 0);
                                assert_eq!(sample.score_calls, 0);
                                assert!(sample.worker_preparation_ns > 0);
                                assert!(sample.ingress_ns > 0);
                            }
                            assert!(sample.allocator.allocations > 0);
                            assert_eq!(
                                sample.model_replacements, 0,
                                "retain the existing VecModel"
                            );
                            assert!(std::ptr::eq(
                                model_identity.as_any(),
                                app.get_search_results().as_any()
                            ));
                            assert!(
                                !app.get_discover_search_animate_results(),
                                "retained result publication cannot rearm entrance"
                            );
                            if let Some(key) = anchor.as_ref() {
                                let at = actual
                                    .iter()
                                    .position(|meta| &unified_search::result_key(meta) == key)
                                    .unwrap();
                                assert_eq!(
                                    app.get_discover_kb_idx() as usize,
                                    at,
                                    "focused owner identity survives staged reorder"
                                );
                                let card = app.get_search_results().row_data(at).unwrap();
                                assert!(card.is_loaded);
                                assert_eq!(
                                    card.poster.to_rgba8().unwrap().as_bytes(),
                                    reference_pixel.as_bytes()
                                );
                            } else {
                                let key = unified_search::result_key(&actual[0]);
                                let model = app.get_search_results();
                                let model = model
                                    .as_any()
                                    .downcast_ref::<VecModel<MediaCard>>()
                                    .unwrap();
                                let mut card = model.row_data(0).unwrap();
                                card.poster = Image::from_rgba8(reference_pixel.clone());
                                card.is_loaded = true;
                                model.set_row_data(0, card);
                                anchor = Some(key);
                            }
                            for (index, meta) in actual.iter().enumerate() {
                                let card = app.get_search_results().row_data(index).unwrap();
                                assert_eq!(card.id.as_str(), meta.id);
                                assert_eq!(card.title.as_str(), meta.title());
                                assert_eq!(card.badge.as_str(), unified_search::card_text(meta).0);
                                assert_eq!(card.year.as_str(), unified_search::card_text(meta).1);
                            }
                            let manifest = serde_json::json!({"providers":providers,"rows":rows,"order":order_name,"stage":stage,"source":source,"signature":expected_signature});
                            if repeat == 0 {
                                use std::io::Write;
                                if let Some(file) = exact_output.as_mut() {
                                    writeln!(file, "{manifest}").unwrap();
                                }
                                if let Some(expected) = expected_output.as_mut() {
                                    assert_eq!(
                                        expected.pop_front().unwrap(),
                                        manifest,
                                        "saved native baseline signature mismatch"
                                    );
                                }
                            }
                            println!(
                                "UI05_CALLBACK {}",
                                serde_json::json!({"providers":providers,"rows":rows,"order":order_name,"repeat":repeat,"stage":stage,"visible":actual.len(),"source":source,"delivery_ns":delivered_at,"signature":nova_providers::catalog_grouping::projection_fingerprint(&expected_signature),"sample":sample})
                            );
                            callbacks.push(sample.callback_ns);
                            waits.push(sample.shared_wait_ns);
                            holds.push(sample.shared_hold_ns);
                            if stage + 1 == order.len() {
                                last_latencies.push(delivered_at);
                            }
                            validation_time += validation_started.elapsed();
                        }
                        first_latencies.push(first.unwrap());
                        // A steady-state repaint must retain model/image/focus and
                        // not restart the entrance before a new 40ms reveal timer.
                        // Let Discover's deferred focus-follow settle naturally.
                        // Under viewport mounting, row zero need not be present:
                        // the retained logical focus is the visible card to test.
                        assert!(app.get_kb_active());
                        assert_eq!(app.get_discover_kb_zone(), 3);
                        assert!(app.get_discover_search_open());
                        assert!(!app.get_show_home());
                        let focused_before = app.get_discover_kb_idx();
                        let focused_key = anchor.as_ref().unwrap();
                        assert_eq!(
                            &unified_search::result_key(
                                &bridge.shared.lock().unwrap().search_previews
                                    [focused_before as usize]
                            ),
                            focused_key,
                        );
                        let focused_label = card_accessible_label(
                            &app.get_search_results()
                                .row_data(focused_before as usize)
                                .unwrap(),
                        );
                        for _ in 0..10 {
                            i_slint_backend_testing::mock_elapsed_time(
                                std::time::Duration::from_millis(100),
                            );
                            if let Some(row) =
                                i_slint_backend_testing::ElementHandle::find_by_accessible_label(
                                    &app,
                                    &focused_label,
                                )
                                .next()
                            {
                                let _ = row.computed_opacity();
                            }
                        }
                        let opacity = || {
                            i_slint_backend_testing::ElementHandle::find_by_accessible_label(
                                &app, &focused_label,
                            )
                            .next()
                            .unwrap_or_else(|| panic!(
                                "headless Discover instantiates the retained focused delegate: label={focused_label:?}, focused_index={}, search_open={}, scope={}, search_scroll={}, rows={}",
                                app.get_discover_kb_idx(), app.get_discover_search_open(), app.get_search_scope(),
                                app.get_discover_search_scroll_y(), app.get_search_results().row_count(),
                            ))
                            .computed_opacity()
                        };
                        assert_eq!(app.get_discover_kb_idx(), focused_before);
                        assert!(
                            opacity() > 0.99,
                            "settled focused search delegate is visible"
                        );
                        let prior = signature(&bridge.shared.lock().unwrap().search_previews);
                        let browse_scroll = app.get_discover_scroll_y();
                        let search_scroll = app.get_discover_search_scroll_y();
                        let repaint = deliver(&bridge, generation, Vec::new());
                        assert_eq!(
                            signature(&bridge.shared.lock().unwrap().search_previews),
                            prior
                        );
                        assert_eq!(
                            repaint.model_moves
                                + repaint.model_inserts
                                + repaint.model_removes
                                + repaint.model_replacements,
                            0
                        );
                        assert!(!app.get_discover_search_animate_results());
                        assert_eq!(app.get_discover_kb_idx(), focused_before);
                        assert_eq!(
                            app.get_discover_scroll_y(),
                            browse_scroll,
                            "search repaint preserves independent browse scroll"
                        );
                        assert_eq!(
                            app.get_discover_search_scroll_y(),
                            search_scroll,
                            "search repaint preserves its settled focus-follow viewport"
                        );
                        assert!(std::ptr::eq(
                            model_identity.as_any(),
                            app.get_search_results().as_any()
                        ));
                        let key = anchor.as_ref().unwrap();
                        let at = bridge
                            .shared
                            .lock()
                            .unwrap()
                            .search_previews
                            .iter()
                            .position(|meta| &unified_search::result_key(meta) == key)
                            .unwrap();
                        let card = app.get_search_results().row_data(at).unwrap();
                        assert!(card.is_loaded);
                        assert_eq!(
                            card.poster.to_rgba8().unwrap().as_bytes(),
                            reference_pixel.as_bytes()
                        );
                        i_slint_backend_testing::mock_elapsed_time(
                            std::time::Duration::from_millis(20),
                        );
                        assert!(
                            opacity() > 0.99,
                            "retained delegate cannot start another entrance"
                        );
                        assert!(app.get_kb_active());
                        assert_eq!(app.get_discover_kb_zone(), 3);
                        assert_eq!(app.get_discover_kb_idx(), focused_before);
                        assert_eq!(app.get_discover_search_scroll_y(), search_scroll);
                        assert_eq!(app.get_discover_scroll_y(), browse_scroll);
                    }
                    println!(
                        "UI05_SUMMARY {}",
                        serde_json::json!({"providers":providers,"rows":rows,"order":order_name,"repeats":repeats,"callbacks":callbacks.len(),"callback_ns":{"p50":percentile(&callbacks,50),"p95":percentile(&callbacks,95),"p99":percentile(&callbacks,99)},"shared_wait_ns":{"p50":percentile(&waits,50),"p95":percentile(&waits,95),"p99":percentile(&waits,99)},"shared_hold_ns":{"p50":percentile(&holds,50),"p95":percentile(&holds,95),"p99":percentile(&holds,99)},"first_delivery_ns":{"p50":percentile(&first_latencies,50),"p95":percentile(&first_latencies,95),"p99":percentile(&first_latencies,99)},"last_delivery_ns":{"p50":percentile(&last_latencies,50),"p95":percentile(&last_latencies,95),"p99":percentile(&last_latencies,99)}})
                    );
                }
            }
        }
        if let Some(expected) = expected_output {
            assert!(expected.is_empty(), "all saved stages replayed");
        }
        std::fs::write(root.join("completed"), "ok").unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settle_test_projection(bridge: &Bridge) {
        let until = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            profiles::invoke_account(|| slint::quit_event_loop().unwrap()).unwrap();
            slint::run_event_loop().unwrap();
            let state = bridge.shared.lock().unwrap();
            if state.search_projection.published == state.search_projection.requested {
                return;
            }
            assert!(
                std::time::Instant::now() < until,
                "search projection did not settle: requested={} published={} model_revision={} memory_revision={} scope={} published_scope={:?} raw={} basis={} model={}",
                state.search_projection.requested,
                state.search_projection.published,
                state.search_projection.model_revision,
                state.search_projection.memory_revision,
                state.unified_search.projection_local().scope,
                state.search_projection.published_scope,
                state.search_previews.len(),
                state.search_projection.frame.keys.len(),
                bridge.app().unwrap().get_search_results().row_count()
            );
            drop(state);
            std::thread::yield_now();
        }
    }

    fn blocked_projection_invalidation_case(name: &str, case: &str) {
        super::super::profile_actions_tests::isolated(name, |fixture| {
            let bridge = &fixture.bridge;
            let original_owner = profiles::ProfileStore::capture();
            let app = bridge.app().unwrap();
            let bob = (case == "profile").then(|| profiles::create("Projection Bob").unwrap());
            {
                let mut state = bridge.shared.lock().unwrap();
                state.search = "N".into();
                state.search_generation = 81;
                state.search_targets = vec![search_target("owner")];
                state.installed = vec![Installed {
                    url: state.search_targets[0].addon_url.clone(),
                    label: "Fixture".into(),
                    enabled: true,
                    available: true,
                    configure_ok: None,
                    generation: 1,
                    manifest: serde_json::from_value(serde_json::json!({
                        "id":"fixture", "name":"Fixture", "version":"1", "types":["movie"],
                        "resources":[], "catalogs":[]
                    }))
                    .unwrap(),
                }];
            }
            app.set_discover_search_open(true);
            // One-character local query registers the real semantic clock while
            // avoiding a corpus scan; admission still uses the actual callback.
            bridge.refresh_local_search("N");
            settle_test_projection(bridge);
            let (entered, waiting) = std::sync::mpsc::sync_channel(1);
            let (release, blocked) = std::sync::mpsc::sync_channel(1);
            let blocked = Mutex::new(blocked);
            let once = AtomicBool::new(true);
            *SEARCH_PROJECTION_TEST_GATE.lock().unwrap() = Some(Arc::new(move || {
                if once.swap(false, Ordering::AcqRel) {
                    entered.send(()).unwrap();
                    blocked
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(30))
                        .unwrap();
                }
            }));
            struct Reset;
            impl Drop for Reset {
                fn drop(&mut self) {
                    SEARCH_PROJECTION_TEST_GATE.lock().unwrap().take();
                }
            }
            let _reset = Reset;
            bridge.shared.lock().unwrap().search_pending_requests = 1;
            bridge.search_test_admit_page(81, vec![search_meta("tt9911000", "Needle revoked")]);
            waiting.recv_timeout(Duration::from_secs(15)).unwrap();
            match case {
                "query" => bridge.search_edited("M"),
                "scope" => bridge.pick_search_scope(2),
                "profile" => {
                    bridge.switch_profile(bob.as_ref().unwrap());
                    bridge.wait_profile_activation();
                    assert!(app.get_profiles_error().is_empty());
                    bridge.search_edited("M");
                }
                "policy" => {
                    profile_policy::save(
                        &profiles::ProfileStore::capture(),
                        &profile_policy::ProfilePolicy {
                            restricted: true,
                            pin: "1234".into(),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    bridge.repaint_search();
                }
                "removed-addon" => {
                    let mut state = bridge.shared.lock().unwrap();
                    state.installed.clear();
                    state.search_targets.clear();
                    state.search_pending_requests = 0;
                    drop(state);
                    bridge.repaint_search();
                }
                _ => unreachable!(),
            }
            assert_eq!(app.get_search_results().row_count(), 0);
            release.send(()).unwrap();
            settle_test_projection(bridge);
            assert!(
                bridge.shared.lock().unwrap().search_previews.is_empty(),
                "{case}"
            );
            assert_eq!(app.get_search_results().row_count(), 0, "{case}");
            if matches!(case, "query" | "profile") {
                for owner in [original_owner, profiles::ProfileStore::capture()] {
                    let history: Vec<SearchHistoryEntry> = owner
                        .read("discover:search_history:v2")
                        .unwrap()
                        .map(|raw| serde_json::from_str(&raw).unwrap())
                        .unwrap_or_default();
                    assert!(
                        history.iter().all(|entry| entry.query != "N"),
                        "rejected {case} preparation must not write its old query history"
                    );
                }
            }
        });
    }
    macro_rules! blocked_projection_case {
        ($name:ident, $case:literal) => {
            #[test]
            fn $name() {
                blocked_projection_invalidation_case(
                    concat!(module_path!(), "::", stringify!($name)),
                    $case,
                );
            }
        };
    }
    blocked_projection_case!(projection_blocked_search_edit_rejects_old_rows, "query");
    blocked_projection_case!(projection_blocked_scope_change_rejects_old_rows, "scope");
    blocked_projection_case!(
        projection_blocked_profile_switch_rejects_old_rows,
        "profile"
    );
    blocked_projection_case!(projection_blocked_policy_change_rejects_old_rows, "policy");
    blocked_projection_case!(
        projection_blocked_addon_removal_rejects_old_rows,
        "removed-addon"
    );

    #[test]
    fn projection_memory_overlay_keeps_local_cap_and_focused_identity() {
        super::super::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::projection_memory_overlay_keeps_local_cap_and_focused_identity"
            ),
            |fixture| {
                let bridge = &fixture.bridge;
                let app = bridge.app().unwrap();
                let entry = |at: usize| {
                    serde_json::from_value::<LibraryEntry>(serde_json::json!({
                    "id": format!("tt{}", 2000000 + at), "type_":"movie", "name": format!("Needle {at:03}")
                })).unwrap()
                };
                {
                    let mut state = bridge.shared.lock().unwrap();
                    state.search = "Needle".into();
                    state.search_generation = 45;
                    state.search_pending_requests = 1;
                    state.search_targets = vec![search_target("owner")];
                    state.entries = (100..250).map(entry).collect();
                }
                app.set_discover_search_open(true);
                bridge.refresh_local_search("Needle");
                bridge.pick_search_scope(0);
                bridge.search_test_admit_page(45, vec![]);
                settle_test_projection(bridge);
                assert_eq!(app.get_search_results().row_count(), 150);
                app.set_kb_active(true);
                app.set_discover_kb_zone(3);
                app.set_discover_kb_idx(10);
                let focused = app.get_search_results().row_data(10).unwrap().id;
                bridge.shared.lock().unwrap().entries.push(entry(0));
                bridge.repaint_search_memory();
                let model = app.get_search_results();
                assert_eq!(model.row_count(), 150);
                assert_eq!(model.row_data(0).unwrap().title.as_str(), "Needle 000");
                assert_eq!(model.row_data(149).unwrap().title.as_str(), "Needle 248");
                assert_eq!(
                    model
                        .row_data(app.get_discover_kb_idx() as usize)
                        .unwrap()
                        .id,
                    focused
                );
                settle_test_projection(bridge);
                assert_eq!(model.row_count(), 150);
                assert_eq!(model.row_data(0).unwrap().title.as_str(), "Needle 000");
                assert_eq!(model.row_data(149).unwrap().title.as_str(), "Needle 248");
                assert_eq!(
                    model
                        .row_data(app.get_discover_kb_idx() as usize)
                        .unwrap()
                        .id,
                    focused
                );
            },
        );
    }

    #[test]
    fn projection_preserves_admitted_title_and_direct_image_across_replans() {
        super::super::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::projection_preserves_admitted_title_and_direct_image_across_replans"
            ),
            |fixture| {
                let bridge = &fixture.bridge;
                let app = bridge.app().unwrap();
                {
                    let mut state = bridge.shared.lock().unwrap();
                    state.search = "Needle".into();
                    state.search_generation = 44;
                    state.search_pending_requests = 1;
                    state.search_targets = vec![search_target("owner")];
                    state.entries = serde_json::from_value(serde_json::json!([
                        {"id":"tt10001", "type_":"movie", "name":"Needle saved old"}
                    ]))
                    .unwrap();
                }
                app.set_discover_search_open(true);
                bridge.refresh_local_search("Needle");
                bridge.pick_search_scope(0);
                bridge.search_test_admit_page(44, vec![search_meta("tt10001", "Needle provider")]);
                settle_test_projection(bridge);
                assert_eq!(
                    app.get_search_results().row_data(0).unwrap().title.as_str(),
                    "Needle saved old"
                );
                let (entered, waiting) = std::sync::mpsc::sync_channel(1);
                let (release, blocked) = std::sync::mpsc::sync_channel(1);
                let blocked = Mutex::new(blocked);
                let once = AtomicBool::new(true);
                *SEARCH_PROJECTION_TEST_GATE.lock().unwrap() = Some(Arc::new(move || {
                    if once.swap(false, Ordering::AcqRel) {
                        entered.send(()).unwrap();
                        blocked
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(30))
                            .unwrap();
                    }
                }));
                struct Reset;
                impl Drop for Reset {
                    fn drop(&mut self) {
                        SEARCH_PROJECTION_TEST_GATE.lock().unwrap().take();
                    }
                }
                let _reset = Reset;
                bridge.repaint_search();
                waiting.recv_timeout(Duration::from_secs(15)).unwrap();
                let model = app.get_search_results();
                let mut row = model.row_data(0).unwrap();
                row.poster_path = "memory://newer-ui05".into();
                row.poster = slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
                    &[3u8, 7, 11, 255],
                    1,
                    1,
                ));
                row.is_loaded = true;
                model.set_row_data(0, row);
                bridge.shared.lock().unwrap().entries[0].name = "Needle accepted title".into();
                bridge.repaint_search_memory();
                assert_eq!(
                    model.row_data(0).unwrap().title.as_str(),
                    "Needle accepted title"
                );
                assert_eq!(
                    model.row_data(0).unwrap().poster_path.as_str(),
                    "memory://newer-ui05"
                );
                release.send(()).unwrap();
                settle_test_projection(bridge);
                // A second ordinary projection must retain the memory URL too,
                // rather than reverting after its first preservation receipt.
                bridge.repaint_search();
                settle_test_projection(bridge);
                let row = app.get_search_results().row_data(0).unwrap();
                assert_eq!(row.title.as_str(), "Needle accepted title");
                assert_eq!(row.poster_path.as_str(), "memory://newer-ui05");
                assert!(row.is_loaded);
                assert_eq!(row.poster.to_rgba8().unwrap().as_bytes(), &[3, 7, 11, 255]);
                assert_eq!(
                    bridge.shared.lock().unwrap().entries[0].name,
                    "Needle accepted title"
                );
            },
        );
    }

    #[test]
    fn projection_source_authority_change_retracts_running_and_pending_pages() {
        super::super::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::projection_source_authority_change_retracts_running_and_pending_pages"
            ),
            |fixture| {
                let bridge = &fixture.bridge;
                let app = bridge.app().unwrap();
                {
                    let mut state = bridge.shared.lock().unwrap();
                    state.search = "Needle".into();
                    state.search_generation = 41;
                    state.search_pending_requests = 2;
                    state.search_targets = vec![search_target("owner")];
                    state.installed = vec![Installed {
                        url: state.search_targets[0].addon_url.clone(),
                        label: "Fixture".into(),
                        enabled: true,
                        available: true,
                        configure_ok: None,
                        generation: 1,
                        manifest: serde_json::from_value(serde_json::json!({
                            "id":"fixture", "name":"Fixture", "version":"1", "types":["movie"],
                            "resources":[], "catalogs":[]
                        }))
                        .unwrap(),
                    }];
                }
                app.set_discover_search_open(true);
                let (entered, waiting) = std::sync::mpsc::sync_channel(1);
                let (release, blocked) = std::sync::mpsc::sync_channel(1);
                let blocked = Mutex::new(blocked);
                let once = AtomicBool::new(true);
                *SEARCH_PROJECTION_TEST_GATE.lock().unwrap() = Some(Arc::new(move || {
                    if once.swap(false, Ordering::AcqRel) {
                        entered.send(()).unwrap();
                        blocked
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(30))
                            .unwrap();
                    }
                }));
                struct Reset;
                impl Drop for Reset {
                    fn drop(&mut self) {
                        SEARCH_PROJECTION_TEST_GATE.lock().unwrap().take();
                    }
                }
                let _reset = Reset;
                bridge.search_test_admit_page(
                    41,
                    vec![search_meta("tt10001", "Needle revoked active")],
                );
                waiting.recv_timeout(Duration::from_secs(15)).unwrap();
                bridge.search_test_admit_page(
                    41,
                    vec![search_meta("tt10002", "Needle revoked queued")],
                );
                // No query/search generation change: authority alone invalidates
                // both the running accumulator and the coalesced old page.
                bridge.shared.lock().unwrap().installed[0].generation = 2;
                bridge.repaint_search();
                assert_eq!(app.get_search_results().row_count(), 0);
                release.send(()).unwrap();
                let until = std::time::Instant::now() + Duration::from_secs(15);
                loop {
                    profiles::invoke_account(|| slint::quit_event_loop().unwrap()).unwrap();
                    slint::run_event_loop().unwrap();
                    let state = bridge.shared.lock().unwrap();
                    if state.search_projection.published == state.search_projection.requested {
                        break;
                    }
                    drop(state);
                    assert!(
                        std::time::Instant::now() < until,
                        "authority projection did not settle"
                    );
                    std::thread::yield_now();
                }
                let state = bridge.shared.lock().unwrap();
                assert!(state.search_previews.is_empty());
                assert!(state.search_targets[0].results.is_empty());
                assert_eq!(state.search_pending_requests, 0);
                assert_eq!(state.search_projection.last_sources[0].generation, 2);
                assert_eq!(app.get_search_results().row_count(), 0);
            },
        );
    }

    #[test]
    fn projection_header_cache_isolated_across_equal_metadata_stamps() {
        let mut worker = SearchProjectionProcessor::default();
        let owner = |instance| ("same-profile".into(), 7, instance, 11, "Needle".into());
        let typed = ("movie".into(), "tt123".into());
        let numeric_stamp = (19, 0);
        assert!(worker.select_owner(owner(1)));
        worker.header_stamp = Some(numeric_stamp);
        worker
            .headers
            .insert(typed.clone(), Some("old-owner-url".into()));
        worker.header_order.push_back(typed.clone());
        worker.header_bytes = 100;
        assert!(!worker.select_owner(owner(1)));
        assert!(worker.headers.contains_key(&typed));
        assert!(worker.select_owner(owner(2)));
        assert!(worker.headers.is_empty());
        assert!(worker.header_order.is_empty());
        assert_eq!(worker.header_bytes, 0);
        assert_eq!(worker.header_stamp, None);
        // Equal numeric revisions in the new owner cannot hit the old URL.
        worker.header_stamp = Some(numeric_stamp);
        worker
            .headers
            .insert(typed.clone(), Some("new-owner-url".into()));
        let source = SearchSourceReceipt {
            url: "https://owner.example".into(),
            generation: 1,
            enabled: true,
            available: true,
        };
        assert!(
            worker
                .replace_sources(std::slice::from_ref(&source))
                .is_some()
        );
        assert!(worker.headers.is_empty());
        worker.header_stamp = Some(numeric_stamp);
        worker
            .headers
            .insert(typed.clone(), Some("before-authority-change".into()));
        let mut replaced = source;
        replaced.generation = 2;
        assert!(
            worker
                .replace_sources(std::slice::from_ref(&replaced))
                .is_some()
        );
        assert!(worker.headers.is_empty());
        assert_eq!(worker.header_stamp, None);
    }

    #[test]
    fn projection_plan_preserves_legacy_edit_sequence() {
        fn key(value: usize) -> SearchResultKey {
            ("movie".into(), value.to_string(), String::new())
        }
        fn reference(old: &[SearchResultKey], new: &[SearchResultKey]) -> Vec<SearchModelEdit> {
            let mut keys = old.to_vec();
            let mut edits = Vec::new();
            for (at, key) in new.iter().enumerate() {
                if keys.get(at) != Some(key) {
                    if let Some(from) = keys
                        .iter()
                        .enumerate()
                        .skip(at + 1)
                        .find_map(|(index, old)| (old == key).then_some(index))
                    {
                        let key = keys.remove(from);
                        keys.insert(at, key);
                        edits.push(SearchModelEdit::Move(from, at));
                    } else {
                        keys.insert(at, key.clone());
                        edits.push(SearchModelEdit::Insert(at));
                    }
                }
                edits.push(SearchModelEdit::Set(at));
            }
            while keys.len() > new.len() {
                keys.pop();
                edits.push(SearchModelEdit::Remove(keys.len()));
            }
            edits
        }
        let mut seed = 0xa0761d6478bd642fu64;
        for _ in 0..2000 {
            let mut old = Vec::new();
            let mut new = Vec::new();
            for value in 0..48 {
                seed ^= seed << 7;
                seed ^= seed >> 9;
                if seed & 1 != 0 {
                    old.push(key(value));
                }
                if seed & 2 != 0 {
                    new.push(key(value));
                }
            }
            for at in (1..new.len()).rev() {
                seed ^= seed << 7;
                seed ^= seed >> 9;
                let other = seed as usize % (at + 1);
                new.swap(at, other);
            }
            assert_eq!(search_edit_plan(&old, &new), reference(&old, &new));
        }
    }

    #[test]
    fn projection_entry_capture_tracks_searchable_deltas_only() {
        let mut view = SearchProjectionView::default();
        let mut entry: LibraryEntry = serde_json::from_value(serde_json::json!({
            "id": "tt-ui05", "type_": "movie", "name": "Signal", "year": "2020",
            "poster_url": "old"
        }))
        .unwrap();
        view.capture_entries(std::slice::from_ref(&entry));
        let revision = view.memory_revision;
        assert!(
            !view
                .entry_deltas
                .values()
                .next()
                .unwrap()
                .as_ref()
                .unwrap()
                .poster_admitted
        );
        view.capture_entries(std::slice::from_ref(&entry));
        assert_eq!(view.memory_revision, revision);
        entry.categories.push("New category".into());
        view.capture_entries(std::slice::from_ref(&entry));
        assert_eq!(view.memory_revision, revision);
        entry.name = "Signal changed".into();
        view.capture_entries(std::slice::from_ref(&entry));
        assert_eq!(view.memory_revision, revision + 1);
        entry.poster_url = "new".into();
        view.capture_entries(std::slice::from_ref(&entry));
        let typed = ("movie".into(), "tt-ui05".into());
        assert!(view.entry_deltas[&typed].as_ref().unwrap().poster_admitted);
        view.capture_entries(&[]);
        assert!(view.entry_deltas[&typed].is_none());
    }

    fn matrix_distance_reference(left: &[char], right: &[char]) -> usize {
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

    fn matrix_search_score_reference(query: &str, title: &str, year: Option<&str>) -> u32 {
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
                    matrix_similarity_reference(query_token, title_token)
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
                best_similarity = best_similarity.max(matrix_similarity_reference(&query, &window));
            }
        }
        if best_similarity >= 750 {
            550_000 + best_similarity * 100
        } else {
            0
        }
    }

    fn matrix_similarity_reference(left: &str, right: &str) -> u32 {
        let left: Vec<char> = left.chars().collect();
        let right: Vec<char> = right.chars().collect();
        let max_len = left.len().max(right.len());
        if max_len == 0 {
            return 1_000;
        }
        let distance = matrix_distance_reference(&left, &right).min(max_len);
        ((max_len - distance) * 1_000 / max_len) as u32
    }

    #[test]
    fn fuzzy_three_rows_match_matrix_exhaustively() {
        let mut strings = vec![Vec::new()];
        for length in 1..=5 {
            for bits in 0..1usize << length {
                strings.push(
                    (0..length)
                        .map(|bit| if bits & (1 << bit) == 0 { 'a' } else { 'b' })
                        .collect(),
                );
            }
        }
        let mut pairs = 0;
        for left in &strings {
            for right in &strings {
                assert_eq!(
                    damerau_levenshtein(left, right),
                    matrix_distance_reference(left, right),
                    "{left:?} / {right:?}"
                );
                pairs += 1;
            }
        }
        assert_eq!(pairs, 3_969);
    }

    #[test]
    fn fuzzy_three_rows_match_matrix_for_unicode_and_random_sequences() {
        for (left, right, expected) in [
            ("", "", 0),
            ("", "猫🙂", 2),
            ("a", "", 1),
            ("abc", "abxc", 1),
            ("abc", "ac", 1),
            ("abc", "axc", 1),
            ("ab", "ba", 1),
            ("aab", "aba", 1),
            ("猫🙂", "🙂猫", 1),
            ("café", "cafe\u{301}", 2),
            ("é", "e\u{301}", 2),
            ("CA", "ABC", 3), // OSA differs from unrestricted Damerau here.
            ("👩\u{200d}💻", "💻\u{200d}👩", 2),
        ] {
            let a: Vec<_> = left.chars().collect();
            let b: Vec<_> = right.chars().collect();
            assert_eq!(
                damerau_levenshtein(&a, &b),
                expected,
                "{left:?} / {right:?}"
            );
            assert_eq!(
                damerau_levenshtein(&a, &b),
                matrix_distance_reference(&a, &b)
            );
            assert_eq!(
                normalized_edit_similarity(left, right),
                matrix_similarity_reference(left, right)
            );
        }
        let alphabet = [
            'a',
            'b',
            'é',
            '\u{301}',
            '猫',
            '🙂',
            '\u{200d}',
            'Ж',
            'ß',
            '\u{10ffff}',
            '\0',
        ];
        let mut seed = 0x0a51_6d27_f389_42bcu64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as usize
        };
        for _ in 0..4_096 {
            let left_len = next() % 97;
            let right_len = next() % 97;
            let left: Vec<_> = (0..left_len)
                .map(|_| alphabet[next() % alphabet.len()])
                .collect();
            let right: Vec<_> = (0..right_len)
                .map(|_| alphabet[next() % alphabet.len()])
                .collect();
            assert_eq!(
                damerau_levenshtein(&left, &right),
                matrix_distance_reference(&left, &right),
                "{left:?} / {right:?}"
            );
            assert_eq!(
                damerau_levenshtein(&right, &left),
                matrix_distance_reference(&left, &right)
            );
        }
    }

    #[test]
    fn fuzzy_three_rows_preserve_thousand_title_scores_and_stable_ranking() {
        let titles = [
            "Star Wars",
            "Star Trek",
            "Spider Man",
            "Amélie",
            "猫の映画",
            "Život",
            "Movie",
            "Repeated repeated",
            "🙂 Adventure",
            "Straße",
        ];
        let corpus: Vec<_> = (0..1_000)
            .map(|index| {
                (
                    titles[index % titles.len()],
                    format!("{}", 1980 + index % 47),
                )
            })
            .collect();
        for query in [
            "satr wars",
            "star war",
            "wars star",
            "spiderman",
            "Amelie",
            "猫の映",
            "žviot",
            "Movie 2020",
            "Repeated",
            "🙂 adventrue",
            "STRASSE",
            "",
            "ab",
            "a",
        ] {
            let new_scores: Vec<_> = corpus
                .iter()
                .map(|(title, year)| search_match_score(query, title, Some(year)))
                .collect();
            let old_scores: Vec<_> = corpus
                .iter()
                .map(|(title, year)| matrix_search_score_reference(query, title, Some(year)))
                .collect();
            assert_eq!(new_scores, old_scores, "{query}");
            let mut new_order: Vec<_> = (0..corpus.len()).collect();
            let mut old_order = new_order.clone();
            new_order.sort_by_key(|&index| std::cmp::Reverse(new_scores[index]));
            old_order.sort_by_key(|&index| std::cmp::Reverse(old_scores[index]));
            assert_eq!(new_order, old_order, "{query}");
        }
    }

    fn grouping_preview(id: &str, parent: &str, season: u32, owner: &str) -> MetaPreview {
        serde_json::from_value(serde_json::json!({"id":id,"type":"series","name":id,"poster":format!("https://images.invalid/{id}.jpg"),"novaSourceUrl":owner,"novaCatalogGrouping":{"version":1,"kind":"series_member","parentIds":[parent],"season":season}})).unwrap()
    }

    fn grouping_key(owner: &str) -> CatalogGroupingKey {
        CatalogGroupingKey {
            addon_url: owner.into(),
            type_: "series".into(),
            catalog_id: "anime".into(),
            surface: CatalogSurface::Discover,
        }
    }

    #[test]
    fn grouped_backfill_distinguishes_novel_collapsed_repeated_empty_failed_and_budget_pages() {
        let key = grouping_key("https://owner.invalid");
        let mut pages = PageState::default();
        for index in 0..GROUPING_BACKFILL_PAGES {
            assert!(pages.accept(
                key.clone(),
                &[grouping_preview(
                    &format!("mal:{index}"),
                    "tt1",
                    1,
                    &key.addon_url
                )],
                index > 0
            ));
            let fetch = pages.should_backfill(CatalogGroupingMode::Series, 1, 1, true);
            assert_eq!(fetch, index + 1 < GROUPING_BACKFILL_PAGES);
        }
        assert!(pages.paused());
        assert!(!pages.terminal());
        assert!(pages.resume());
        assert_eq!(pages.pages_in_wave, 0);
        assert!(!pages.accept(
            key.clone(),
            &[grouping_preview("mal:1", "tt1", 1, &key.addon_url)],
            true
        ));
        assert_eq!(pages.stop, PageStop::Repeated);
        assert!(pages.terminal());
        assert!(!pages.resume());
        assert!(!pages.accept(key.clone(), &[], false));
        assert_eq!(pages.stop, PageStop::Empty);
        pages.stop = PageStop::Failed;
        assert!(pages.resume());
        assert_eq!(pages.stop, PageStop::Ready);
        // Reprojection with unchanged full visible content is not a collapsed page.
        assert!(!pages.should_backfill(CatalogGroupingMode::Series, 40, 40, false));
        assert!(!pages.should_backfill(CatalogGroupingMode::Original, 1, 1, true));
    }

    #[test]
    fn raw_catalog_dedup_uses_exact_owner_type_and_native_identity() {
        let first = grouping_preview("opaque:one", "tt1", 1, "https://first.invalid/key");
        let second = grouping_preview("opaque:one", "tt1", 1, "https://second.invalid/key");
        let mut raw = vec![];
        append_catalog_raw(&mut raw, vec![first.clone(), second.clone(), first]);
        assert_eq!(raw.len(), 2);
        assert_eq!(
            catalog_source_identity(&raw[0]).addon_url,
            "https://first.invalid/key"
        );
        assert_eq!(
            catalog_source_identity(&raw[1]).addon_url,
            "https://second.invalid/key"
        );
        let mut pages = PageState::default();
        let key = grouping_key("https://first.invalid/key");
        assert!(pages.accept(key.clone(), &raw, false));
        raw.reverse();
        assert!(
            !pages.accept(key, &raw, true),
            "a reordered repeated response is still a repeated page"
        );
    }

    #[test]
    fn focused_member_and_virtual_season_remap_to_owning_group() {
        let one = SourceIdentity {
            addon_url: "https://one.invalid/key".into(),
            media_type: "series".into(),
            id: "native:1".into(),
        };
        let two = SourceIdentity {
            id: "native:2".into(),
            ..one.clone()
        };
        let other_owner = SourceIdentity {
            addon_url: "https://two.invalid/key".into(),
            ..two.clone()
        };
        let anchor = CatalogAnchor {
            index: 5,
            display_key: "old".into(),
            members: vec![two.clone()],
            season: Some(2),
        };
        let groups = [
            ("wrong-owner".into(), vec![other_owner], Some(2)),
            ("season-one".into(), vec![one.clone(), two.clone()], Some(1)),
            ("season-two".into(), vec![one, two], Some(2)),
        ];
        assert_eq!(locate_catalog_anchor(&anchor, &groups), 2);
        let series = [("parent".into(), groups[2].1.clone(), None)];
        assert_eq!(locate_catalog_anchor(&anchor, &series), 0);
    }

    #[test]
    fn grouping_discover_reprojects_raw_keeps_owner_cursor_focus_and_pixels() {
        const CHILD: &str = "NOVA_TEST_CATALOG_GROUPING_CHILD";
        let Ok(root) = std::env::var(CHILD) else {
            let root =
                std::env::temp_dir().join(format!("nova-catalog-grouping-{}", std::process::id()));
            let result = std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","app::catalog::tests::grouping_discover_reprojects_raw_keeps_owner_cursor_focus_and_pixels","--nocapture"]).env(CHILD,&root).output().unwrap();
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
            Bridge::new(
                app.as_weak(),
                Arc::new(AtomicU64::new(7)),
                player,
                downloads,
            )
        };
        #[cfg(not(feature = "desktop"))]
        let bridge = Bridge::new(
            app.as_weak(),
            Arc::new(AtomicU64::new(7)),
            player,
            downloads,
        );
        let owner = "https://owner.invalid/secret";
        {
            let mut state = bridge.shared.lock().unwrap();
            state.type_defs = vec![TypeDef {
                type_: "series".into(),
                label: "Series".into(),
                catalogs: vec![CatDef {
                    id: "anime".into(),
                    addon_url: owner.into(),
                    supports_skip: false,
                    genre_options: vec![],
                    label: "Same addon label".into(),
                    addon_label: "Same addon label".into(),
                }],
            }];
            state.cache_settings.catalog_grouping_default = CatalogGroupingMode::Series;
        }
        let first = grouping_preview("mal:1", "tt1", 1, owner);
        let second = grouping_preview("kitsu:2", "tt1", 2, owner);
        bridge.apply_catalog(7, vec![first.clone(), second], false);
        assert_eq!(app.get_catalog().row_count(), 1);
        let old_key = catalog_grouping::display_key(&bridge.shared.lock().unwrap().previews[0]);
        app.invoke_set_card_poster(
            0,
            Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::new(1, 1)),
        );
        assert!(app.get_catalog().row_data(0).unwrap().is_loaded);
        bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .catalog_grouping_default = CatalogGroupingMode::Original;
        bridge.reproject_catalog_grouping_discover();
        assert_eq!(app.get_catalog().row_count(), 2);
        assert!(app.get_catalog().row_data(0).unwrap().is_loaded);
        assert!(!bridge.catalog_poster_slot_matches(
            7,
            0,
            &old_key,
            first.poster.as_deref().unwrap()
        ));
        app.set_discover_kb_idx(1);
        bridge.apply_catalog(
            7,
            vec![first, grouping_preview("anilist:3", "tt3", 1, owner)],
            true,
        );
        {
            let state = bridge.shared.lock().unwrap();
            assert_eq!(
                state.next_skip, 4,
                "raw response count includes an exact-ID duplicate"
            );
            assert_eq!(state.catalog_raw.len(), 3);
            assert_eq!(state.previews.len(), 3);
            assert_eq!(catalog_source_identity(&state.previews[1]).addon_url, owner);
        }
        bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .catalog_grouping_default = CatalogGroupingMode::Series;
        bridge.reproject_catalog_grouping_discover();
        assert_eq!(app.get_catalog().row_count(), 2);
        assert_eq!(
            app.get_discover_kb_idx(),
            0,
            "a source season follows its grouped parent"
        );
        assert!(app.get_catalog().row_data(0).unwrap().is_loaded);
        bridge.apply_catalog(6, vec![grouping_preview("stale", "tt99", 1, owner)], true);
        bridge.catalog_page_failed(6);
        assert_eq!(bridge.shared.lock().unwrap().next_skip, 4);
        bridge.catalog_page_failed(7);
        assert_eq!(bridge.shared.lock().unwrap().next_skip, 4);
        assert_eq!(
            bridge.shared.lock().unwrap().catalog_page.stop,
            PageStop::Failed
        );
        assert!(app.get_discover_pagination_paused());
        bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .catalog_grouping_default = CatalogGroupingMode::Original;
        bridge.reproject_catalog_grouping_discover();
        assert_eq!(app.get_catalog().row_count(), 3);
        assert_eq!(bridge.shared.lock().unwrap().catalog_raw[1].id, "kitsu:2");
    }

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
            Bridge::new(
                app.as_weak(),
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

    fn refresh_test_addon(url: &str) -> Installed {
        Installed {
            url: url.into(),
            label: "Episodes".into(),
            enabled: true,
            configure_ok: None,
            available: true,
            generation: 1,
            manifest: serde_json::from_value(serde_json::json!({
                "id":"episodes", "name":"Episodes", "version":"1", "types":["series"],
                "resources":["meta"], "catalogs":[]
            }))
            .unwrap(),
        }
    }

    fn refresh_test_entry(state: &mut Shared, id: &str, watched_at: u64) {
        state.entries.push(
            serde_json::from_value(serde_json::json!({
                "id":id, "type_":"series", "name":id, "description":"Already cached"
            }))
            .unwrap(),
        );
        let episode_id = format!("{id}:2:1");
        state.progress.insert(
            progress_map_key(id, &episode_id),
            EpisodeProgress {
                series_id: id.into(),
                episode_id,
                watched: true,
                updated_at_secs: watched_at,
                ..Default::default()
            },
        );
    }

    #[test]
    fn saved_episode_refresh_bounds_work_rotates_and_prioritizes_overdue_active_shows() {
        let mut state = Shared::default();
        state
            .installed
            .push(refresh_test_addon("https://episodes.invalid"));
        let now = std::time::Instant::now();
        let wall = 100 * 24 * 3600;
        refresh_test_entry(&mut state, "active", wall);
        for index in 0..500 {
            refresh_test_entry(&mut state, &format!("old-{index:03}"), 1);
        }
        let first = reserve_saved_episode_refresh(&mut state, 1, now, wall);
        assert_eq!(first.len(), 4);
        assert_eq!(first[0].id, "active");
        assert_eq!(first[0].interval, SAVED_EPISODE_RECENT_INTERVAL);
        assert_eq!(first[1].interval, SAVED_EPISODE_OLD_INTERVAL);
        assert!(
            reserve_saved_episode_refresh(&mut state, 2, now + Duration::from_secs(3600), wall)
                .is_empty(),
            "pending work coalesces even across inventory revisions"
        );
        for job in &first {
            state
                .metadata_prefetch
                .get_mut(&(job.type_.clone(), job.id.clone()))
                .unwrap()
                .retry_at = Some(now + job.interval);
        }
        assert!(
            reserve_saved_episode_refresh(&mut state, 1, now + Duration::from_secs(299), wall)
                .is_empty()
        );
        let second =
            reserve_saved_episode_refresh(&mut state, 1, now + SAVED_EPISODE_BATCH_INTERVAL, wall);
        assert_eq!(second.len(), 4);
        assert!(
            second
                .iter()
                .all(|job| first.iter().all(|old| old.id != job.id))
        );
        for job in &second {
            state
                .metadata_prefetch
                .get_mut(&(job.type_.clone(), job.id.clone()))
                .unwrap()
                .retry_at = Some(now + job.interval);
        }
        let third =
            reserve_saved_episode_refresh(&mut state, 1, now + SAVED_EPISODE_RECENT_INTERVAL, wall);
        assert_eq!(third.len(), 4);
        assert_eq!(
            third[0].id, "active",
            "hundreds of unvisited old titles cannot starve an overdue active show"
        );
        assert!(
            third.iter().skip(1).all(|job| first
                .iter()
                .chain(second.iter())
                .all(|old| old.id != job.id)),
            "old titles still get their fair share"
        );
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn saved_episode_refresh_discovers_home_without_detail_and_rejects_stale_responses() {
        const ROOT: &str = "NOVA_SAVED_EPISODE_REFRESH_TEST";
        let Ok(root) = std::env::var(ROOT) else {
            let base = std::env::temp_dir().join(format!(
                "nova-episode-refresh-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            for mode in [
                "success",
                "source",
                "removed",
                "profile",
                "empty",
                "superseded",
            ] {
                let root = base.join(mode);
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .env(ROOT, &root).env("NOVA_EPISODE_REFRESH_MODE", mode)
                    .env("XDG_CACHE_HOME", root.join("cache"))
                    .args(["--exact", "app::catalog::tests::saved_episode_refresh_discovers_home_without_detail_and_rejects_stale_responses", "--nocapture"])
                    .output().unwrap();
                assert!(
                    output.status.success(),
                    "{mode}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            fs::remove_dir_all(base).unwrap();
            return;
        };
        use std::io::{Read, Write};
        let mode = std::env::var("NOVA_EPISODE_REFRESH_MODE").unwrap();
        let root = PathBuf::from(root);
        storage::init_at(&root);
        if mode == "profile" {
            profiles::initialize().unwrap();
        }
        i_slint_backend_testing::init_integration_test_with_system_time();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let received = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (seen, ready, stop) = (received.clone(), release.clone(), done.clone());
        let server_mode = mode.clone();
        let server = thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let (mut socket, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = [0; 4096];
                let len = socket.read(&mut request).unwrap();
                assert!(
                    String::from_utf8_lossy(&request[..len])
                        .contains("/meta/series/refresh-show.json")
                );
                seen.fetch_add(1, Ordering::Release);
                while !ready.load(Ordering::Acquire) && !stop.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(5));
                }
                let videos = if server_mode == "empty" {
                    serde_json::json!([])
                } else {
                    serde_json::json!([
                        {"id":"refresh-show:1:1","season":1,"episode":1,"released":"2020-01-01"},
                        {"id":"refresh-show:2:1","season":2,"episode":1,"released":"2020-02-01"},
                        {"id":"refresh-show:2:2","season":2,"episode":2,"released":"2020-02-08"}
                    ])
                };
                let body = serde_json::to_vec(&serde_json::json!({"meta":{"id":"refresh-show","type":"series","name":"Show","videos":videos}})).unwrap();
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                socket.write_all(&body).unwrap();
            }
        });
        let app = AppWindow::new().unwrap();
        app.show().unwrap();
        app.set_show_home(true);
        let player = crate::player::Player::setup(&app);
        let bridge = Bridge::new(
            app.as_weak(),
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        let cached: Vec<Video> = serde_json::from_value(serde_json::json!([
            {"id":"refresh-show:1:1","season":1,"episode":1,"released":"2020-01-01","thumbnail":"https://known.invalid/one.jpg"},
            {"id":"refresh-show:2:1","season":2,"episode":1,"released":"2020-02-01","thumbnail":"https://known.invalid/two.jpg"}
        ])).unwrap();
        // The fixture has complete cached artwork as well as episode data.
        // Otherwise Home's independent Watch history poster-repair path also
        // requests meta, obscuring the refresh scheduler's coalescing check.
        let poster = "https://known.invalid/poster.jpg";
        for url in [
            poster,
            "https://known.invalid/one.jpg",
            "https://known.invalid/two.jpg",
        ] {
            decoded_cache_insert(
                &sized_cache_key(url, Some(DISPLAY_POSTER_SIDE)),
                SharedPixelBuffer::<Rgba8Pixel>::new(1, 1),
            );
        }
        write_episodes_cache_for("series", "refresh-show", &cached);
        write_meta_header_for(
            "series",
            "refresh-show",
            &MetaHeader {
                description: "Cached text".into(),
                poster_url: poster.into(),
                ..Default::default()
            },
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            state.installed.push(refresh_test_addon(&url));
            refresh_test_entry(&mut state, "refresh-show", now_secs());
            state.entries[0].poster_url = poster.into();
            state.progress.insert(
                progress_map_key("refresh-show", "refresh-show:1:1"),
                EpisodeProgress {
                    series_id: "refresh-show".into(),
                    episode_id: "refresh-show:1:1".into(),
                    watched: true,
                    updated_at_secs: now_secs().saturating_sub(1),
                    ..Default::default()
                },
            );
            write_progress_map(&state.progress);
        }
        bridge.persist_library();
        persistence::flush().unwrap();
        bridge.migrate_viewing_activity();
        bridge.rebuild_continue_list();
        bridge.apply_home_to_ui();
        assert_eq!(app.get_home_new_episodes().row_count(), 0);
        assert!(bridge.shared.lock().unwrap().artwork_recovery.is_empty());
        bridge.prefetch_library_meta();
        assert!(
            bridge.shared.lock().unwrap().metadata_prefetch.is_empty(),
            "the old populated-cache prefetch skips this title"
        );
        remote_playback::playback_foreground(false);
        bridge.refresh_saved_episode_metadata();
        assert!(
            bridge.shared.lock().unwrap().metadata_prefetch.is_empty(),
            "background apps do not refresh"
        );
        remote_playback::playback_foreground(true);
        // A real Home revisit starts the refresh without opening Detail.
        bridge.show_home_page();
        bridge.refresh_saved_episode_metadata();
        let weak = app.as_weak();
        let check = bridge.clone();
        let seen = received.clone();
        let ready = release.clone();
        let mode_check = mode.clone();
        let changed = Rc::new(std::cell::Cell::new(None::<std::time::Instant>));
        let started = std::time::Instant::now();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(20),
            move || {
                let app = weak.upgrade().unwrap();
                if seen.load(Ordering::Acquire) > 0 && changed.get().is_none() {
                    match mode_check.as_str() {
                        "source" => check.shared.lock().unwrap().installed[0].generation += 1,
                        "removed" => check.shared.lock().unwrap().entries.clear(),
                        "profile" => {
                            let id = profiles::create("Other").unwrap();
                            let prepared = profiles::prepare_activation(&id).unwrap();
                            let session = check.sessions.prepare(&id, Shared::default());
                            check.sessions.publish(session.clone());
                            profiles::commit_session(prepared, &session);
                        }
                        "superseded" => {
                            let mut newer =
                                read_episodes_cache_for("series", "refresh-show").unwrap();
                            for episode in [2, 3] {
                                newer.push(Video {
                                    id: format!("refresh-show:2:{episode}"),
                                    season: Some(2),
                                    episode: Some(episode),
                                    ..Default::default()
                                });
                            }
                            write_episodes_cache_for("series", "refresh-show", &newer);
                        }
                        _ => {}
                    }
                    changed.set(Some(std::time::Instant::now()));
                    ready.store(true, Ordering::Release);
                }
                let complete = if mode_check == "success" {
                    app.get_home_new_episodes().row_count() == 1
                } else {
                    changed
                        .get()
                        .is_some_and(|at| at.elapsed() > Duration::from_secs(1))
                };
                if complete || started.elapsed() > Duration::from_secs(10) {
                    slint::quit_event_loop().unwrap();
                }
            },
        );
        slint::run_event_loop().unwrap();
        done.store(true, Ordering::Release);
        release.store(true, Ordering::Release);
        server.join().unwrap();
        assert_eq!(
            received.load(Ordering::Acquire),
            1,
            "revisits/ticks must coalesce"
        );
        assert!(!app.get_modal_visible());
        if mode == "success" {
            assert_eq!(
                app.get_home_new_episodes().row_count(),
                1,
                "fresh S2E2 must appear without opening Detail"
            );
            let videos = read_episodes_cache_for("series", "refresh-show").unwrap();
            assert_eq!(videos.len(), 3);
            for old in &cached {
                assert_eq!(
                    videos.iter().find(|v| v.id == old.id).unwrap().thumbnail,
                    old.thumbnail
                );
            }
            bridge.refresh_saved_episode_metadata();
            assert!(
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .metadata_prefetch
                    .values()
                    .all(|job| job.retry_at.is_some())
            );
        } else {
            assert_eq!(app.get_home_new_episodes().row_count(), 0);
            if mode != "profile" {
                assert_eq!(
                    read_episodes_cache_for("series", "refresh-show")
                        .unwrap()
                        .len(),
                    if mode == "superseded" { 4 } else { 2 },
                    "invalid/empty refresh must preserve the cached inventory"
                );
                assert!(
                    bridge
                        .shared
                        .lock()
                        .unwrap()
                        .metadata_prefetch
                        .values()
                        .all(|job| job.retry_at.is_some())
                );
            }
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

    #[test]
    fn search_pagination_stops_identical_and_reordered_pages_after_two_requests() {
        for reverse in [false, true] {
            let mut targets = vec![search_target("repeating")];
            let page: Vec<_> = (0..100)
                .map(|index| search_meta(&format!("opaque-{index}"), "First metadata"))
                .collect();
            record_search_source_page(&mut targets, 0, page.clone());
            assert!(!targets[0].exhausted);
            let mut repeat = page;
            if reverse {
                repeat.reverse();
            }
            for meta in &mut repeat {
                meta.name = "Changed duplicate".into();
            }
            record_search_source_page(&mut targets, 0, repeat);
            assert_eq!(
                targets[0].next_skip, 200,
                "raw provider cursor includes repeated rows"
            );
            assert_eq!(targets[0].results.len(), 100);
            assert!(
                targets[0]
                    .results
                    .iter()
                    .all(|meta| meta.name == "First metadata")
            );
            assert!(targets[0].exhausted);
            assert_eq!(
                targets.iter().filter(|target| !target.exhausted).count(),
                0,
                "a third request is ineligible"
            );
        }
    }

    #[test]
    fn search_pagination_retains_first_metadata_and_advances_raw_overlap_cursor() {
        let mut targets = vec![search_target("overlap")];
        record_search_source_page(
            &mut targets,
            0,
            vec![
                search_meta("a", "First a"),
                search_meta("a", "Duplicate a"),
                search_meta("b", "First b"),
            ],
        );
        assert_eq!(targets[0].results.len(), 2);
        assert_eq!(targets[0].next_skip, 3);
        record_search_source_page(
            &mut targets,
            0,
            vec![
                search_meta("b", "New b"),
                search_meta("c", "First c"),
                search_meta("c", "Duplicate c"),
            ],
        );
        assert_eq!(targets[0].next_skip, 6);
        assert!(!targets[0].exhausted);
        assert_eq!(
            targets[0]
                .results
                .iter()
                .map(|meta| meta.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert_eq!(
            targets[0]
                .results
                .iter()
                .map(|meta| meta.name.as_str())
                .collect::<Vec<_>>(),
            ["First a", "First b", "First c"]
        );
        // This subset never occurred as a page, but has no new identities.
        record_search_source_page(
            &mut targets,
            0,
            vec![search_meta("a", "Again"), search_meta("c", "Again")],
        );
        assert_eq!(targets[0].next_skip, 8);
        assert_eq!(targets[0].results.len(), 3);
        assert!(targets[0].exhausted);
    }

    #[test]
    fn search_pagination_tracks_each_catalogue_and_addon_owner_independently() {
        let mut targets = vec![
            search_target("first"),
            search_target("same-addon-other-catalogue"),
            search_target("different-addon"),
        ];
        targets[1].addon_url = targets[0].addon_url.clone();
        for index in 0..targets.len() {
            record_search_source_page(
                &mut targets,
                index,
                vec![search_meta("local-42", "First title")],
            );
        }
        // The two catalogues share an addon-native identity; the other owner
        // retains its own opaque title even when its ID and name are equal.
        assert_eq!(unique_search_results(&targets).len(), 2);
        record_search_source_page(&mut targets, 0, vec![search_meta("local-42", "Repeated")]);
        assert!(targets[0].exhausted);
        for index in [1, 2] {
            assert!(!targets[index].exhausted);
            record_search_source_page(
                &mut targets,
                index,
                vec![search_meta("new-local", "New title")],
            );
            assert_eq!(targets[index].results.len(), 2);
            assert!(!targets[index].exhausted);
        }
        assert_eq!(unique_search_results(&targets).len(), 4);
        record_search_source_page(&mut targets, 1, Vec::new());
        assert!(targets[1].exhausted);
        assert!(!targets[2].exhausted);
    }

    #[test]
    fn search_pagination_empty_single_page_and_failure_keep_terminal_semantics() {
        let mut targets = vec![
            search_target("empty"),
            search_target("single"),
            search_target("retry"),
        ];
        record_search_page_outcome(&mut targets, 0, Some(Vec::new()));
        assert!(targets[0].exhausted);
        assert_eq!(targets[0].next_skip, 0);
        targets[1].supports_skip = false;
        record_search_page_outcome(
            &mut targets,
            1,
            Some(vec![
                search_meta("a", "First"),
                search_meta("a", "Duplicate"),
            ]),
        );
        assert!(targets[1].exhausted);
        assert_eq!(targets[1].results.len(), 1);
        assert_eq!(targets[1].next_skip, 0);
        record_search_source_page(&mut targets, 2, vec![search_meta("a", "First")]);
        record_search_page_outcome(&mut targets, 2, None);
        assert!(!targets[2].exhausted);
        assert_eq!(targets[2].next_skip, 1);
        record_search_page_outcome(&mut targets, 2, Some(vec![search_meta("b", "Retry")]));
        assert!(!targets[2].exhausted);
        assert_eq!(targets[2].next_skip, 2);
        assert_eq!(targets[2].results.len(), 2);
    }

    #[test]
    fn search_pagination_cursor_saturates_without_discarding_new_rows() {
        let mut targets = vec![search_target("overflow")];
        targets[0].next_skip = usize::MAX - 1;
        record_search_source_page(
            &mut targets,
            0,
            vec![search_meta("a", "A"), search_meta("b", "B")],
        );
        assert_eq!(targets[0].next_skip, usize::MAX);
        assert_eq!(targets[0].results.len(), 2);
        assert!(!targets[0].exhausted);
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
            page_fingerprints: HashSet::new(),
            results: Arc::default(),
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
            Bridge::new(
                app.as_weak(),
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
                search_meta("tt1234567", "Common duplicate"),
                search_meta("second", "Common"),
            ],
        );
        let first_delivery = unique_search_results(&targets);
        assert_eq!(
            first_delivery
                .iter()
                .map(|meta| meta.id.as_str())
                .collect::<Vec<_>>(),
            ["tt1234567", "second"]
        );

        // The preferred source arrives later. Its metadata wins the duplicate,
        // while both previously and newly delivered unique items remain.
        record_search_source_page(
            &mut targets,
            0,
            vec![
                search_meta("tt1234567", "Common"),
                search_meta("first", "Common"),
            ],
        );
        let complete = ranked_search_results("Common", &targets);
        assert_eq!(
            complete
                .iter()
                .map(|meta| meta.id.as_str())
                .collect::<Vec<_>>(),
            ["tt1234567", "first", "second"]
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

#[cfg(test)]
mod poster_range_tests {
    use super::*;

    #[test]
    fn poster_retry_window_prioritizes_reported_rows_and_bounds_initial_or_stale_ranges() {
        let initial = PageState::default();
        assert_eq!(initial.poster_retry_range(320), 0..=31);
        assert_eq!(initial.poster_retry_range(5), 0..=4);
        let reported = PageState {
            visible_first: 0,
            visible_last: 8,
            ..Default::default()
        };
        assert_eq!(reported.poster_retry_range(320), 0..=8);
        let stale = PageState {
            visible_first: 999,
            visible_last: 999,
            ..Default::default()
        };
        assert_eq!(stale.poster_retry_range(4), 3..=3);
        let wide = PageState {
            visible_first: 0,
            visible_last: 319,
            ..Default::default()
        };
        assert_eq!(wide.poster_retry_range(320), 0..=127);
    }
}

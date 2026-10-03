//! Home page: featured catalogs, Continue Watching, and Upcoming.
use super::*;

const HOME_SHOWCASE_PER_CATALOG: usize = 5;
const HOME_SHOWCASE_CACHE_KEY: &str = "home:showcase:v1";
type HomeShowcaseBatch = (HomeCatalogSource, Option<Vec<MetaPreview>>);

/// Device-local catalog results, keyed by the complete configured selection.
/// Failed requests keep the previous batch; only successful responses replace
/// it. Artwork uses the ordinary image cache rather than a second disk copy.
#[derive(Default, Serialize, Deserialize)]
struct HomeShowcaseCache {
    catalogs: Vec<HomeShowcaseCatalog>,
}

#[derive(Serialize, Deserialize)]
struct HomeShowcaseCatalog {
    source: HomeCatalogSource,
    previews: Vec<MetaPreview>,
}

impl HomeShowcaseCache {
    fn previews(&self, sources: &[HomeCatalogSource]) -> Vec<MetaPreview> {
        merge_home_showcase_results(
            sources
                .iter()
                .enumerate()
                .filter_map(|(index, source)| {
                    let cached = self
                        .catalogs
                        .iter()
                        .find(|cached| &cached.source == source)?;
                    Some((index, cached.previews.clone()))
                })
                .collect(),
        )
    }

    fn update(&mut self, selected: &[HomeCatalogSource], batches: Vec<HomeShowcaseBatch>) {
        self.catalogs
            .retain(|cached| selected.contains(&cached.source));
        // Keep enough candidates to fill five slots after deduplication with
        // every earlier selected catalog, without persisting entire pages.
        let limit =
            HOME_SHOWCASE_PER_CATALOG * selected.iter().collect::<HashSet<_>>().len().max(1);
        for (source, previews) in batches {
            let Some(previews) = previews.filter(|_| selected.contains(&source)) else {
                continue;
            };
            let mut seen = HashSet::new();
            let previews = previews
                .into_iter()
                .filter(|preview| {
                    !preview.id.is_empty()
                        && !preview.title().trim().is_empty()
                        && seen.insert((preview.type_.clone(), preview.id.clone()))
                })
                .take(limit)
                .collect();
            let cached = HomeShowcaseCatalog { source, previews };
            if let Some(index) = self
                .catalogs
                .iter()
                .position(|entry| entry.source == cached.source)
            {
                self.catalogs[index] = cached;
            } else {
                self.catalogs.push(cached);
            }
        }
    }
}

fn home_showcase_sources(state: &Shared) -> Vec<HomeCatalogSource> {
    let mut seen = HashSet::new();
    state
        .cache_settings
        .home_catalog_sources
        .iter()
        .filter(|source| {
            seen.insert((*source).clone())
                && state
                    .installed
                    .iter()
                    .any(|addon| addon.url == source.addon_url && addon.enabled)
        })
        .cloned()
        .collect()
}

fn parse_home_showcase_catalog(bytes: &[u8], type_: &str) -> Option<Vec<MetaPreview>> {
    // A 200 response carrying an addon error or an empty stub is a failed
    // refresh, rather than proof that a previously cached catalog is empty.
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value.get("metas")?.as_array()?;
    let mut previews = Addon::parse_catalog(bytes).ok()?;
    for preview in &mut previews {
        if preview.type_.is_empty() {
            preview.type_ = type_.into();
        }
    }
    Some(previews)
}

impl Bridge {
    /// Load a small first page from each selected Home catalog. Each catalog
    /// contributes at most five distinct titles; Discover's current picker,
    /// pagination and search state are intentionally untouched.
    pub(super) fn refresh_home_showcase(&self) {
        let generation = self.home_showcase_gen.fetch_add(1, Ordering::Relaxed) + 1;
        let (selected, sources, targets, restore) = {
            let mut state = self.shared.lock().unwrap();
            state.home_showcase_loaded = true;
            let sources = home_showcase_sources(&state);
            let restore = state.home_showcase_sources != sources || state.home_showcase.is_empty();
            if restore {
                state.home_showcase_sources = sources.clone();
                state.home_showcase_refresh = None;
            }
            let targets = sources
                .iter()
                .filter_map(|source| {
                    let addon = state.installed.iter().find(|addon| {
                        addon.url == source.addon_url && addon.enabled && addon.available
                    })?;
                    addon
                        .manifest
                        .catalog_for(&source.type_, &source.catalog_id)?;
                    let addon = Addon::new(&addon.url).ok()?;
                    let extra = if source.genre.is_empty() {
                        Vec::new()
                    } else {
                        vec![("genre", source.genre.as_str())]
                    };
                    Some((
                        source.clone(),
                        addon.catalog_url(&source.type_, &source.catalog_id, &extra),
                    ))
                })
                .collect::<Vec<_>>();
            (
                state.cache_settings.home_catalog_sources.clone(),
                sources,
                targets,
                restore,
            )
        };
        let mut cached =
            read_json::<HomeShowcaseCache>(HOME_SHOWCASE_CACHE_KEY).unwrap_or_default();
        let previous_count = cached.catalogs.len();
        cached.update(&selected, Vec::new());
        if cached.catalogs.len() != previous_count {
            write_json(HOME_SHOWCASE_CACHE_KEY, &cached);
        }
        if restore {
            self.install_home_showcase(cached.previews(&sources));
        }
        if targets.is_empty() {
            return;
        }

        let results = Arc::new(Mutex::new(Vec::<HomeShowcaseBatch>::new()));
        let remaining = Arc::new(AtomicUsize::new(targets.len()));
        for (source, url) in targets {
            let bridge = self.clone();
            let results = results.clone();
            let remaining = remaining.clone();
            let sources = sources.clone();
            net::fetch_bytes(url, move |result| {
                let previews = result
                    .ok()
                    .and_then(|bytes| parse_home_showcase_catalog(&bytes, &source.type_));
                results.lock().unwrap().push((source, previews));
                if remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
                    let batches = std::mem::take(&mut *results.lock().unwrap());
                    let _ = slint::invoke_from_event_loop(move || {
                        bridge.finish_home_showcase_refresh(generation, sources, batches);
                    });
                }
            });
        }
    }

    /// Invalidate any in-flight response when addon/catalog preferences
    /// change. The next visit to Home starts a fresh bounded fetch.
    pub(super) fn invalidate_home_showcase(&self) {
        self.home_showcase_gen.fetch_add(1, Ordering::Relaxed);
        {
            let mut state = self.shared.lock().unwrap();
            state.home_showcase_loaded = false;
            state.home_showcase_refresh = None;
        }
        if let Some(app) = self.app() {
            app.set_home_featured_refresh_pending(false);
        }
    }

    pub(super) fn ensure_home_showcase_loaded(&self) {
        let refresh = {
            let state = self.shared.lock().unwrap();
            !state.home_showcase_loaded
                || home_showcase_sources(&state) != state.home_showcase_sources
        };
        if refresh {
            self.refresh_home_showcase();
        }
    }

    fn finish_home_showcase_refresh(
        &self,
        generation: u64,
        sources: Vec<HomeCatalogSource>,
        batches: Vec<HomeShowcaseBatch>,
    ) {
        if generation != self.home_showcase_gen.load(Ordering::Relaxed) {
            return;
        }
        let selected = {
            let state = self.shared.lock().unwrap();
            if home_showcase_sources(&state) != sources {
                return;
            }
            state.cache_settings.home_catalog_sources.clone()
        };
        let mut cached =
            read_json::<HomeShowcaseCache>(HOME_SHOWCASE_CACHE_KEY).unwrap_or_default();
        cached.update(&selected, batches);
        write_json(HOME_SHOWCASE_CACHE_KEY, &cached);
        let previews = cached.previews(&sources);
        if self.shared.lock().unwrap().home_showcase.is_empty() {
            self.install_home_showcase(previews);
        } else {
            let prefetch = {
                let mut state = self.shared.lock().unwrap();
                if serde_json::to_value(&state.home_showcase).ok()
                    == serde_json::to_value(&previews).ok()
                {
                    state.home_showcase_refresh = None;
                    None
                } else {
                    let index = home_showcase_refreshed_index(&state, &previews, 1);
                    let backdrop = previews
                        .get(index)
                        .and_then(|preview| preview.background.clone());
                    state.home_showcase_refresh = Some(previews);
                    backdrop
                }
            };
            if let Some(app) = self.app() {
                app.set_home_featured_refresh_pending(
                    self.shared.lock().unwrap().home_showcase_refresh.is_some(),
                );
            }
            if let Some(url) = prefetch.filter(|url| !url.is_empty()) {
                // Warm the ordinary image cache without changing visible
                // state. The next index change can commit cached pixels.
                net::fetch_image(url, None, |_| {});
            }
        }
    }

    fn install_home_showcase(&self, previews: Vec<MetaPreview>) {
        let (first, count) = {
            let mut state = self.shared.lock().unwrap();
            replace_home_showcase_list(&mut state, previews, 0);
            state.home_showcase_pending_index = (!state.home_showcase.is_empty()).then_some(0);
            state.home_showcase_displayed = state.home_showcase.first().cloned();
            (
                state.home_showcase.first().cloned(),
                state.home_showcase.len(),
            )
        };
        let Some(first) = first else {
            if let Some(app) = self.app() {
                app.set_home_featured_refresh_pending(false);
                app.set_home_featured_title(SharedString::default());
                app.set_home_featured_type(SharedString::default());
                app.set_home_featured_rating(SharedString::default());
                app.set_home_featured_runtime(SharedString::default());
                app.set_home_featured_release_info(SharedString::default());
                app.set_home_featured_tagline(SharedString::default());
                app.set_home_featured_description(SharedString::default());
                app.set_home_featured_backdrop(Image::default());
                app.set_home_featured_index(0);
                app.set_home_featured_count(0);
                app.set_home_featured_revision(app.get_home_featured_revision().wrapping_add(1));
            }
            return;
        };

        // Publish the first title immediately; its backdrop is fetched before
        // it is considered ready for rotation.
        if let Some(app) = self.app() {
            app.set_home_featured_refresh_pending(false);
            app.set_home_featured_title(SharedString::from(first.title()));
            app.set_home_featured_type(SharedString::from(&first.type_));
            app.set_home_featured_rating(SharedString::from(Self::showcase_rating(&first)));
            app.set_home_featured_runtime(SharedString::from(
                first.runtime.as_deref().unwrap_or_default(),
            ));
            app.set_home_featured_release_info(first.year_str().unwrap_or_default().into());
            app.set_home_featured_tagline(
                first
                    .extra
                    .get("tagline")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .into(),
            );
            app.set_home_featured_description(SharedString::from(
                first.description.as_deref().unwrap_or_default(),
            ));
            let backdrop = first.background.as_deref().and_then(decoded_cache_get);
            app.set_home_featured_backdrop(backdrop.map(Image::from_rgba8).unwrap_or_default());
            app.set_home_featured_index(0);
            app.set_home_featured_count(count as i32);
            app.set_home_featured_revision(app.get_home_featured_revision().wrapping_add(1));
        }
        let generation = self.shared.lock().unwrap().home_showcase_list_generation;
        self.ensure_home_showcase_art(0, generation);
    }

    /// Begin loading the requested slide's art, while keeping the currently
    /// displayed slide intact. The next slide is prefetched after every commit.
    fn ensure_home_showcase_art(&self, index: usize, generation: u64) {
        let (backdrop, ready) = {
            let mut state = self.shared.lock().unwrap();
            if generation != state.home_showcase_list_generation {
                return;
            }
            let Some(preview) = state.home_showcase.get(index) else {
                return;
            };
            let backdrop_url = preview.background.clone().unwrap_or_default();
            let artwork = state.home_showcase_artwork.entry(index).or_default();
            if !artwork.backdrop_done
                && !backdrop_url.is_empty()
                && let Some(pixels) = decoded_cache_get(&backdrop_url)
            {
                artwork.backdrop = Some(pixels);
                artwork.backdrop_done = true;
            }
            if backdrop_url.is_empty() {
                artwork.backdrop_done = true;
            }
            let backdrop = if !backdrop_url.is_empty()
                && !artwork.backdrop_done
                && !artwork.backdrop_loading
            {
                artwork.backdrop_loading = true;
                Some(backdrop_url)
            } else {
                None
            };
            (backdrop, artwork.backdrop_done)
        };

        if ready {
            let should_commit =
                self.shared.lock().unwrap().home_showcase_pending_index == Some(index);
            if should_commit {
                self.commit_home_showcase_index(index, generation);
            }
        }
        if let Some(url) = backdrop {
            self.fetch_home_showcase_artwork(index, generation, url);
        }
    }

    fn fetch_home_showcase_artwork(&self, index: usize, generation: u64, url: String) {
        let bridge = self.clone();
        // Keep the add-on-provided source resolution for the large hero rather
        // than using the smaller detail-banner derivative.
        net::fetch_image(url, None, move |pixels| {
            let _ = slint::invoke_from_event_loop(move || {
                bridge.finish_home_showcase_artwork(index, generation, pixels);
            });
        });
    }

    fn finish_home_showcase_artwork(
        &self,
        index: usize,
        generation: u64,
        pixels: Option<SharedPixelBuffer<Rgba8Pixel>>,
    ) {
        let should_commit = {
            let mut state = self.shared.lock().unwrap();
            if generation != state.home_showcase_list_generation {
                return;
            }
            let pending = state.home_showcase_pending_index == Some(index);
            let Some(artwork) = state.home_showcase_artwork.get_mut(&index) else {
                return;
            };
            artwork.backdrop_loading = false;
            artwork.backdrop_done = true;
            artwork.backdrop = pixels;
            pending && artwork.backdrop_done
        };
        if should_commit {
            self.commit_home_showcase_index(index, generation);
        }
    }

    fn commit_home_showcase_index(&self, index: usize, generation: u64) {
        let (preview, backdrop, count) = {
            let mut state = self.shared.lock().unwrap();
            if generation != state.home_showcase_list_generation {
                return;
            }
            if state.home_showcase_pending_index != Some(index) {
                return;
            }
            let Some(artwork) = state.home_showcase_artwork.get(&index) else {
                return;
            };
            if !artwork.backdrop_done {
                return;
            }
            let Some(preview) = state.home_showcase.get(index).cloned() else {
                return;
            };
            let backdrop = artwork.backdrop.clone();
            let count = state.home_showcase.len();
            state.home_showcase_index = index;
            state.home_showcase_pending_index = None;
            state.home_showcase_displayed = Some(preview.clone());
            (preview, backdrop, count)
        };

        if let Some(app) = self.app() {
            app.set_home_featured_title(SharedString::from(preview.title()));
            app.set_home_featured_type(SharedString::from(&preview.type_));
            app.set_home_featured_rating(SharedString::from(Self::showcase_rating(&preview)));
            app.set_home_featured_runtime(SharedString::from(
                preview.runtime.as_deref().unwrap_or_default(),
            ));
            app.set_home_featured_release_info(preview.year_str().unwrap_or_default().into());
            app.set_home_featured_tagline(
                preview
                    .extra
                    .get("tagline")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .into(),
            );
            app.set_home_featured_description(SharedString::from(
                preview.description.as_deref().unwrap_or_default(),
            ));
            app.set_home_featured_backdrop(backdrop.map(Image::from_rgba8).unwrap_or_default());
            app.set_home_featured_index(index as i32);
            app.set_home_featured_count(count as i32);
            app.set_home_featured_revision(app.get_home_featured_revision().wrapping_add(1));
        }

        if count > 1 {
            self.ensure_home_showcase_art((index + 1) % count, generation);
        }
    }

    pub(super) fn home_showcase_step(&self, delta: i32) {
        if delta == 0 {
            return;
        }
        let (index, generation, empty) = {
            let mut state = self.shared.lock().unwrap();
            let index = if let Some(previews) = state.home_showcase_refresh.take() {
                let index = home_showcase_refreshed_index(&state, &previews, delta);
                replace_home_showcase_list(&mut state, previews, index);
                index
            } else {
                let count = state.home_showcase.len();
                if count < 2 {
                    return;
                }
                let base = state
                    .home_showcase_pending_index
                    .unwrap_or(state.home_showcase_index);
                (base as i64 + delta as i64).rem_euclid(count as i64) as usize
            };
            let empty = state.home_showcase.is_empty();
            state.home_showcase_pending_index = (!empty).then_some(index);
            (index, state.home_showcase_list_generation, empty)
        };
        if let Some(app) = self.app() {
            app.set_home_featured_refresh_pending(false);
        }
        if empty {
            self.install_home_showcase(Vec::new());
            return;
        }
        self.ensure_home_showcase_art(index, generation);
    }

    pub(super) fn home_showcase_picked(&self) {
        self.open_home_showcase(false);
    }

    pub(super) fn home_showcase_watch_now(&self) {
        self.open_home_showcase(true);
    }

    fn open_home_showcase(&self, watch_now: bool) {
        let preview = {
            let state = self.shared.lock().unwrap();
            state.home_showcase_displayed.clone()
        };
        if let Some(preview) = preview {
            self.open_preview(preview, 0, false, None, watch_now);
        }
    }

    fn showcase_rating(preview: &MetaPreview) -> String {
        let Some(rating) = preview.rating_str() else {
            return String::new();
        };
        match rating.parse::<f32>() {
            // Stremio's IMDb catalog values use a 0–10 scale; the showcase uses
            // the compact 0–100 score shown in its rating pill.
            Ok(value) if (0.0..=10.0).contains(&value) => format!("{:.0}", value * 10.0),
            _ => rating,
        }
    }

    /// Rebuild [`Shared::continue_list`] from the progress map: one entry per
    /// library item with resumable playback — a series' in-progress or next
    /// not-yet-watched episode, or a movie still in progress. An in-progress
    /// episode (started, not watched) is offered as-is, whatever its air
    /// date; once the latest episode is finished, the next dated, released,
    /// not-yet-watched episode in order is offered instead (progress 0 =
    /// "not started yet"). Dateless episodes never surface on their own. Movies have no "next",
    /// so a watched movie is dropped. Series with nothing left to watch are
    /// skipped — Home → Upcoming covers the unaired tail. Items the user
    /// explicitly removed are skipped until a newer progress update (a resume)
    /// appears; see [`Shared::continue_hidden`].
    pub(super) fn rebuild_continue_list(&self) {
        // Home is a glanceable landing page, not a second library: keep
        // the grid bounded no matter how many series carry progress.
        const CONTINUE_MAX: usize = 20;
        let (entries, progress, hidden) = {
            let state = self.shared.lock().unwrap();
            (
                state.entries.clone(),
                state.progress.clone(),
                state.continue_hidden.clone(),
            )
        };
        // Newest progress entry per item, whatever its state: the latest
        // activity decides whether we resume it or move to the next episode.
        let mut latest: HashMap<String, &EpisodeProgress> = HashMap::new();
        for p in progress.values() {
            let newer = latest
                .get(&p.series_id)
                .map(|b| p.updated_at_secs > b.updated_at_secs)
                .unwrap_or(true);
            if newer {
                latest.insert(p.series_id.clone(), p);
            }
        }
        let mut list: Vec<ContinueEntry> = Vec::new();
        for e in &entries {
            let Some(p) = latest.get(&e.id) else { continue };
            // Explicitly removed from Home: stays hidden until a newer progress
            // update (a resume, local or from a synced peer) appears.
            if hidden.get(&e.id).is_some_and(|at| *at >= p.updated_at_secs) {
                continue;
            }
            let episodes = if e.type_ == "movie" {
                None
            } else {
                read_episodes_cache_for(&e.type_, &e.id)
            };
            let Some(episode_id) =
                continue_resume_id(&e.type_, &e.id, p, episodes.as_deref(), &progress)
            else {
                continue;
            };
            list.push(ContinueEntry {
                series_id: e.id.clone(),
                type_: e.type_.clone(),
                episode_id,
                updated_at_secs: p.updated_at_secs,
            });
        }
        list.sort_by_key(|c| std::cmp::Reverse(c.updated_at_secs));
        list.truncate(CONTINUE_MAX);
        self.shared.lock().unwrap().continue_list = list;

        // Drop hide entries that no longer hide anything (progress advanced
        // past them, or the item left the library), so the local map and the
        // synced `continue_hidden` domain don't accumulate forever. The write
        // tombstones the removed keys mesh-wide.
        let stale: Vec<String> = hidden
            .iter()
            .filter(|(id, at)| {
                latest
                    .get(id.as_str())
                    .map(|p| p.updated_at_secs > **at)
                    .unwrap_or(true)
            })
            .map(|(id, _)| id.clone())
            .collect();
        if !stale.is_empty() {
            let map = {
                let mut state = self.shared.lock().unwrap();
                for id in &stale {
                    state.continue_hidden.remove(id);
                }
                state.continue_hidden.clone()
            };
            write_continue_hidden(&map);
        }
    }

    /// Build the Home → Continue Watching rows (joined against library
    /// entries for title/poster and the episode cache for the label).
    /// Posters paint instantly when already decoded, otherwise dispatch.
    pub(super) fn current_continue_rows(&self) -> Vec<ContinueRow> {
        let (entries, progress, list, enabled, show_unwatched_thumbs) = {
            let state = self.shared.lock().unwrap();
            (
                state.entries.clone(),
                state.progress.clone(),
                state.continue_list.clone(),
                state.cache_settings.home_continue_enabled,
                state.cache_settings.show_unwatched_thumbs,
            )
        };
        if !enabled {
            return Vec::new();
        }
        list.iter()
            .filter_map(|c| {
                let e = entries.iter().find(|e| e.id == c.series_id)?;
                let episodes = read_episodes_cache_for(&c.type_, &c.series_id).unwrap_or_default();
                let video = episodes.iter().find(|v| v.id == c.episode_id);
                let subtitle = video
                    .map(|v| format!("{} · {}", episode_badge(v), episode_row_label(v)))
                    .unwrap_or_else(|| text::tr("Resume").to_string());
                let record = progress.get(&progress_map_key(&c.series_id, &c.episode_id));
                let fraction = record
                    .map(|p| progress_fraction(p.position_secs, p.duration_secs))
                    .unwrap_or(0.0);
                let started = record
                    .map(|p| !p.watched && p.position_secs > 0.0)
                    .unwrap_or(false);
                let badge = {
                    let is_watched = |v: &Video| {
                        progress
                            .get(&progress_map_key(&c.series_id, &v.id))
                            .is_some_and(|p| p.watched)
                    };
                    continue_badge(started, &c.episode_id, &episodes, is_watched)
                };
                // Respect the unwatched-thumbnail preference while still
                // allowing artwork for episodes the user has already started.
                let show_thumbnail = show_unwatched_thumbs
                    || record.is_some_and(|p| p.watched || p.position_secs > 0.0);
                let art_url = home_card_art_url(&e.poster_url, video, show_thumbnail);
                let (poster, is_loaded) = home_card_image(&art_url, &e.poster_url);
                Some(ContinueRow {
                    id: SharedString::from(&c.series_id),
                    title: SharedString::from(&e.name),
                    subtitle: SharedString::from(&subtitle),
                    episode_title: video.map(episode_row_label).unwrap_or_default().into(),
                    ep_no: video.map(episode_badge).unwrap_or_default().into(),
                    remaining: continue_remaining(record).into(),
                    art_url: art_url.into(),
                    poster,
                    is_loaded,
                    progress: fraction,
                    badge,
                })
            })
            .collect()
    }

    /// Push the current Continue Watching + Upcoming rows to the Home page,
    /// plus the Upcoming calendar models (healed first, so a rebuild that
    /// moved air dates never leaves the calendar on a dead month/day).
    pub(super) fn apply_home_to_ui(&self) {
        if let Some(app) = self.app() {
            app.set_home_continue(Rc::new(VecModel::from(self.current_continue_rows())).into());
            app.set_home_upcoming(Rc::new(VecModel::from(self.current_upcoming_rows())).into());
        }
        self.apply_upcoming_cal_to_ui();
    }

    /// Refresh language-dependent Home row text without replacing poster
    /// images or disturbing the carousel models when their shape is unchanged.
    pub(super) fn refresh_home_language_text(&self) {
        let Some(app) = self.app() else { return };

        let continue_rows = self.current_continue_rows();
        let continue_model = app.get_home_continue();
        if continue_model.row_count() == continue_rows.len() {
            for (index, fresh) in continue_rows.into_iter().enumerate() {
                if let Some(mut current) = continue_model.row_data(index)
                    && (current.subtitle != fresh.subtitle || current.remaining != fresh.remaining)
                {
                    current.subtitle = fresh.subtitle;
                    current.episode_title = fresh.episode_title;
                    current.remaining = fresh.remaining;
                    continue_model.set_row_data(index, current);
                }
            }
        } else {
            app.set_home_continue(Rc::new(VecModel::from(continue_rows)).into());
        }

        let upcoming_rows = self.current_upcoming_rows();
        let upcoming_model = app.get_home_upcoming();
        if upcoming_model.row_count() == upcoming_rows.len() {
            for (index, fresh) in upcoming_rows.into_iter().enumerate() {
                if let Some(mut current) = upcoming_model.row_data(index)
                    && (current.subtitle != fresh.subtitle || current.date != fresh.date)
                {
                    current.subtitle = fresh.subtitle;
                    current.date = fresh.date;
                    current.episode_title = fresh.episode_title;
                    upcoming_model.set_row_data(index, current);
                }
            }
        } else {
            app.set_home_upcoming(Rc::new(VecModel::from(upcoming_rows)).into());
        }

        let day = app.get_home_cal_epoch();
        let day_rows = if day >= 0 {
            self.current_upcoming_day_rows(day as i64)
        } else {
            Vec::new()
        };
        let day_model = app.get_home_cal_day();
        if day_model.row_count() == day_rows.len() {
            for (index, fresh) in day_rows.into_iter().enumerate() {
                if let Some(mut current) = day_model.row_data(index)
                    && (current.subtitle != fresh.subtitle || current.date != fresh.date)
                {
                    current.subtitle = fresh.subtitle;
                    current.date = fresh.date;
                    current.episode_title = fresh.episode_title;
                    day_model.set_row_data(index, current);
                }
            }
        } else {
            app.set_home_cal_day(Rc::new(VecModel::from(day_rows)).into());
        }

        let first = {
            let state = self.shared.lock().unwrap();
            if state.upcoming_cal.first == 0 {
                month_first(today_days())
            } else {
                state.upcoming_cal.first
            }
        };
        let (year, month, _) = civil_from_days(first);
        app.set_home_cal_title(SharedString::from(text::cal_month_title(month, year)));
    }

    /// Rebuild [`Shared::upcoming_list`]: unaired episodes of library
    /// series the user caught up with — every dated, released episode
    /// watched, at least one episode watched at all, and at least one
    /// episode airing in the future. Dateless episodes are invisible to this
    /// check (unknown schedule). Air-date ascending, capped so Home stays
    /// a quick glance.
    pub(super) fn rebuild_upcoming_list(&self) {
        const UPCOMING_MAX: usize = 20;
        let (entries, progress) = {
            let state = self.shared.lock().unwrap();
            (state.entries.clone(), state.progress.clone())
        };
        let today = today_days();
        let mut upcoming: Vec<UpcomingEntry> = Vec::new();
        for e in &entries {
            if e.type_ == "movie" {
                continue;
            }
            let episodes = read_episodes_cache_for(&e.type_, &e.id).unwrap_or_default();
            if episodes.is_empty() {
                continue;
            }
            let is_watched = |v: &Video| {
                progress
                    .get(&progress_map_key(&e.id, &v.id))
                    .is_some_and(|p| p.watched)
            };
            let (future, available, available_watched) =
                upcoming_tally(&episodes, is_watched, today);
            // Caught up (something actually watched, nothing available
            // left) and still waiting on unaired episodes.
            if future.is_empty() || available == 0 || available_watched < available {
                continue;
            }
            for (i, days) in future {
                let v = &episodes[i];
                upcoming.push(UpcomingEntry {
                    series_id: e.id.clone(),
                    type_: e.type_.clone(),
                    episode_id: v.id.clone(),
                    air_days: days,
                });
            }
        }
        upcoming.sort_by_key(|u| u.air_days);
        upcoming.truncate(UPCOMING_MAX);
        self.shared.lock().unwrap().upcoming_list = upcoming;
    }

    /// Build the Home → Upcoming rows (same joins as Continue Watching,
    /// plus the human air date). `index` positions each row in the full
    /// Upcoming list, so filtered views (the calendar's selected day) still
    /// resolve picks.
    pub(super) fn current_upcoming_rows(&self) -> Vec<UpcomingRow> {
        let (entries, list, date_relative, enabled, show_unwatched_thumbs) = {
            let state = self.shared.lock().unwrap();
            (
                state.entries.clone(),
                state.upcoming_list.clone(),
                state.cache_settings.date_relative,
                state.cache_settings.home_upcoming_enabled,
                state.cache_settings.show_unwatched_thumbs,
            )
        };
        if !enabled {
            return Vec::new();
        }
        list.iter()
            .enumerate()
            .filter_map(|(i, u)| upcoming_row(&entries, u, i, date_relative, show_unwatched_thumbs))
            .collect()
    }

    /// The Upcoming rows airing on one calendar day (epoch days). Same joins
    /// as [`Self::current_upcoming_rows`]; `index` still positions into the
    /// full Upcoming list, so card taps resolve through `upcoming_picked`.
    pub(super) fn current_upcoming_day_rows(&self, day: i64) -> Vec<UpcomingRow> {
        let (entries, list, date_relative, enabled, show_unwatched_thumbs) = {
            let state = self.shared.lock().unwrap();
            (
                state.entries.clone(),
                state.upcoming_list.clone(),
                state.cache_settings.date_relative,
                state.cache_settings.home_upcoming_enabled,
                state.cache_settings.show_unwatched_thumbs,
            )
        };
        if !enabled {
            return Vec::new();
        }
        list.iter()
            .enumerate()
            .filter(|(_, u)| u.air_days == day)
            .filter_map(|(i, u)| upcoming_row(&entries, u, i, date_relative, show_unwatched_thumbs))
            .collect()
    }

    /// Main thread: an Upcoming card was picked — open the series detail
    /// page (the episode hasn't aired, so there are no streams to jump
    /// to; the user lands on the episode list).
    pub(super) fn upcoming_picked(&self, index: usize) {
        let series_id = {
            self.shared
                .lock()
                .unwrap()
                .upcoming_list
                .get(index)
                .map(|u| u.series_id.clone())
        };
        let Some(series_id) = series_id else { return };
        let lib_idx = self
            .current_library_view()
            .iter()
            .position(|e| e.id == series_id);
        let Some(lib_idx) = lib_idx else { return };
        self.open_library_item(lib_idx);
    }

    /// Main thread: a Continue Watching card was picked — open the series
    /// detail page and jump straight to the resume episode's streams when
    /// its episode list is cached (otherwise the detail page opens
    /// normally and the user picks the episode once meta loads).
    pub(super) fn continue_picked(&self, index: usize) {
        let entry = self.continue_entry(index);
        let Some(c) = entry else { return };
        let lib_idx = self.continue_library_index(&c.series_id);
        let Some(lib_idx) = lib_idx else { return };
        self.open_library_item(lib_idx);
        // Resolve the resume episode against the cached list (the rows
        // just built come from this same cache, so the index matches).
        let (season, row_idx) = {
            let state = self.shared.lock().unwrap();
            let m = match state.modal_item.as_ref() {
                Some(m) if m.id == c.series_id && !m.seasons.is_empty() => m,
                _ => return,
            };
            let videos = read_episodes_cache_for(&c.type_, &c.series_id).unwrap_or_default();
            let v = match videos.iter().find(|v| v.id == c.episode_id) {
                Some(v) => v,
                None => return,
            };
            let season = v.season.unwrap_or(0);
            if !m.seasons.contains(&season) {
                return;
            }
            match Self::filtered_row_index(&videos, season, "", &c.episode_id) {
                Some(i) => (season, i),
                None => return,
            }
        };
        {
            let mut state = self.shared.lock().unwrap();
            if let Some(m) = state.modal_item.as_mut()
                && m.id == c.series_id
            {
                m.season_index = m.seasons.iter().position(|&s| s == season).unwrap_or(0);
            }
        }
        if let Some(app) = self.app() {
            let idx = {
                self.shared
                    .lock()
                    .unwrap()
                    .modal_item
                    .as_ref()
                    .map(|m| m.season_index)
                    .unwrap_or(0)
            };
            app.set_season_combo_idx(idx as i32);
            app.set_detail_kb_ci(0);
            app.set_detail_kb_ep(0);
        }
        self.refresh_episode_rows();
        self.episode_picked(row_idx);
        // Deep-link: the streams view opened without the episode list being
        // visited, so system back must close the modal instead of revealing
        // the skipped list (see `kb_back` in detail.slint). Gate on the
        // streams view actually being active: `episode_picked` no-ops when
        // the episode no longer resolves (e.g. the cache moved on).
        if let Some(app) = self.app() {
            let on_streams = !app.get_modal_episodes() && !app.get_episode_context().is_empty();
            let still_same = self
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .is_some_and(|m| m.id == c.series_id);
            if on_streams && still_same {
                app.set_detail_deep_stream(true);
            }
        }
    }

    /// Main thread: remove the Continue Watching card at `index` until playback
    /// of that item resumes. Stamps the hide time, persists it, and syncs the
    /// hide mesh-wide (the `continue_hidden` domain).
    pub(super) fn continue_remove(&self, index: usize) {
        let id = {
            self.shared
                .lock()
                .unwrap()
                .continue_list
                .get(index)
                .map(|c| c.series_id.clone())
        };
        let Some(id) = id else { return };
        let map = {
            let mut state = self.shared.lock().unwrap();
            state.continue_hidden.insert(id, now_secs());
            state.continue_hidden.clone()
        };
        write_continue_hidden(&map);
        self.rebuild_continue_list();
        self.apply_home_to_ui();
    }

    /// Continue Watching row at `index` (cloned), or `None` when the list
    /// changed under an in-flight menu.
    fn continue_entry(&self, index: usize) -> Option<ContinueEntry> {
        self.shared
            .lock()
            .unwrap()
            .continue_list
            .get(index)
            .cloned()
    }

    /// Index of a Continue Watching card's entry in the current (filtered)
    /// library view: the open/enter/remove actions all work on the card's
    /// library entry, so they resolve it the same way here.
    fn continue_library_index(&self, entry_id: &str) -> Option<usize> {
        self.current_library_view()
            .iter()
            .position(|e| e.id == entry_id)
    }

    /// Main thread: "Enter series" on a Continue Watching card menu — open the
    /// entry's detail page without starting playback (`continue_picked`, the
    /// card tap, is the resume-and-play variant).
    pub(super) fn continue_enter(&self, index: usize) {
        let Some(entry) = self.continue_entry(index) else {
            return;
        };
        let Some(lib_idx) = self.continue_library_index(&entry.series_id) else {
            return;
        };
        self.open_library_item(lib_idx);
    }

    pub(super) fn show_discover_page(&self) {
        if let Some(app) = self.app() {
            app.set_show_home(false);
            app.set_show_library(false);
            app.set_show_settings(false);
        }
    }

    /// Main thread: show the Home landing page (Continue Watching).
    pub(super) fn show_home_page(&self) {
        if let Some(app) = self.app() {
            app.set_show_home(true);
            app.set_show_library(false);
            app.set_show_settings(false);
            // Tapping Home always lands on the carousels, not a remembered
            // "see all" subpage.
            app.set_home_view(0);
        }
        self.rebuild_continue_list();
        self.rebuild_upcoming_list();
        self.apply_home_to_ui();
        self.dispatch_continue_posters();
        self.dispatch_upcoming_posters();
        self.ensure_home_showcase_loaded();
    }

    // ---- Upcoming calendar -------------------------------------------
    //
    // The "see all" Upcoming subpage toggles between the episode grid and a
    // month calendar: 42 cells (full weeks, Monday-first) with episode counts,
    // plus the selected day's episodes underneath. The backend owns the month
    // arithmetic and the selection — Slint only renders cells and forwards
    // taps — so the grid never shifts shape and picks resolve through the
    // rows' full-list `index`.

    /// Push the calendar models to the Home page: the 42 month-grid cells,
    /// the month title, the selected day's episode rows, the selected epoch
    /// (-1 when nothing is selected) and the open flag. Cheap (one filter
    /// pass), so every calendar action just mutates [`Shared::upcoming_cal`]
    /// and re-pushes through here.
    pub(super) fn apply_upcoming_cal_to_ui(&self) {
        let Some(app) = self.app() else { return };
        let (cells, title, epoch, open) = {
            let mut state = self.shared.lock().unwrap();
            heal_upcoming_cal(&mut state);
            let cal = state.upcoming_cal.clone();
            let mut counts: HashMap<i64, usize> = HashMap::new();
            for u in &state.upcoming_list {
                *counts.entry(u.air_days).or_insert(0) += 1;
            }
            let today = today_days();
            let cells = cal_cells(cal.first, &counts, cal.day, today);
            let (year, month, _) = civil_from_days(cal.first);
            (
                cells,
                text::cal_month_title(month, year),
                cal.day.unwrap_or(-1),
                cal.open,
            )
        };
        let day_rows = if epoch >= 0 {
            self.current_upcoming_day_rows(epoch)
        } else {
            Vec::new()
        };
        app.set_home_cal_cells(Rc::new(VecModel::from(cells)).into());
        app.set_home_cal_title(SharedString::from(&title));
        app.set_home_cal_day(Rc::new(VecModel::from(day_rows)).into());
        app.set_home_cal_epoch(epoch as i32);
        app.set_home_cal_open(open);
    }

    /// Main thread: open the Upcoming calendar (the subpage's Calendar
    /// button). Healing picks today / the earliest air date, so opening
    /// always lands on a live month.
    pub(super) fn upcoming_cal_open(&self) {
        self.shared.lock().unwrap().upcoming_cal.open = true;
        self.apply_upcoming_cal_to_ui();
    }

    /// Main thread: back to the episode grid (button, system back).
    pub(super) fn upcoming_cal_close(&self) {
        self.shared.lock().unwrap().upcoming_cal.open = false;
        self.apply_upcoming_cal_to_ui();
    }

    /// Main thread: step the visible month (`delta` = ±1). The selection
    /// follows into the new month when it airs anything there, otherwise it
    /// stays put (still valid) while the grid shows the browsed month.
    pub(super) fn upcoming_cal_month(&self, delta: i32) {
        {
            let mut state = self.shared.lock().unwrap();
            let (year, month, _) = civil_from_days(state.upcoming_cal.first);
            let shifted = (month as i64 - 1) + delta as i64;
            let first = days_from_civil(
                year + shifted.div_euclid(12),
                (shifted.rem_euclid(12) + 1) as u32,
                1,
            );
            state.upcoming_cal.first = first;
            if let Some(day) = state
                .upcoming_list
                .iter()
                .map(|u| u.air_days)
                .filter(|d| month_first(*d) == first)
                .min()
            {
                state.upcoming_cal.day = Some(day);
            }
        }
        self.apply_upcoming_cal_to_ui();
    }

    /// Main thread: pick a calendar day (epoch days). Only days with episodes
    /// stick; the grid only sends those anyway.
    pub(super) fn upcoming_cal_pick(&self, epoch: i32) {
        {
            let mut state = self.shared.lock().unwrap();
            let day = epoch as i64;
            if state.upcoming_list.iter().any(|u| u.air_days == day) {
                state.upcoming_cal.day = Some(day);
            }
        }
        self.apply_upcoming_cal_to_ui();
    }

    /// Main thread: keyboard Enter on the calendar opens the selected day's
    /// first episode (no-op with no selection).
    pub(super) fn upcoming_cal_activate(&self) {
        let idx = {
            let state = self.shared.lock().unwrap();
            state
                .upcoming_cal
                .day
                .and_then(|d| state.upcoming_list.iter().position(|u| u.air_days == d))
        };
        if let Some(i) = idx {
            self.upcoming_picked(i);
        }
    }
}

/// Swap the list at a navigation boundary, reusing completed art by URL. A
/// separate list generation rejects artwork callbacks whose indices belonged
/// to the old list, without cancelling metadata refreshes or valid old art.
fn replace_home_showcase_list(state: &mut Shared, previews: Vec<MetaPreview>, index: usize) {
    let mut cached_art = HashMap::new();
    for (old_index, artwork) in std::mem::take(&mut state.home_showcase_artwork) {
        if artwork.backdrop_done
            && artwork.backdrop.is_some()
            && let Some(url) = state
                .home_showcase
                .get(old_index)
                .and_then(|preview| preview.background.clone())
        {
            cached_art.insert(url, artwork);
        }
    }
    state.home_showcase_artwork = previews
        .iter()
        .enumerate()
        .filter_map(|(index, preview)| {
            let artwork = cached_art.remove(preview.background.as_deref()?);
            artwork.map(|artwork| (index, artwork))
        })
        .collect();
    state.home_showcase_index = index.min(previews.len().saturating_sub(1));
    state.home_showcase = previews;
    state.home_showcase_list_generation = state.home_showcase_list_generation.wrapping_add(1);
    state.home_showcase_refresh = None;
}

fn home_showcase_refreshed_index(state: &Shared, previews: &[MetaPreview], delta: i32) -> usize {
    if previews.is_empty() {
        return 0;
    }
    let base = state
        .home_showcase_pending_index
        .unwrap_or(state.home_showcase_index);
    let current = state
        .home_showcase
        .get(base)
        .or(state.home_showcase_displayed.as_ref());
    let base = current
        .and_then(|current| {
            previews
                .iter()
                .position(|preview| preview.id == current.id && preview.type_ == current.type_)
        })
        .unwrap_or(base);
    (base as i64 + delta as i64).rem_euclid(previews.len() as i64) as usize
}

/// Preserve the settings order of selected catalogs, cap each at five distinct
/// titles, and avoid showing the same `(type, id)` twice when catalogs overlap.
fn merge_home_showcase_results(mut batches: Vec<(usize, Vec<MetaPreview>)>) -> Vec<MetaPreview> {
    batches.sort_by_key(|(index, _)| *index);
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for (_, previews) in batches {
        let mut added = 0;
        for preview in previews {
            if preview.id.is_empty() || preview.title().trim().is_empty() {
                continue;
            }
            if seen.insert((preview.type_.clone(), preview.id.clone())) {
                merged.push(preview);
                added += 1;
                if added == HOME_SHOWCASE_PER_CATALOG {
                    break;
                }
            }
        }
    }
    merged
}

/// Use an episode thumbnail when allowed, otherwise the library poster.
/// The selected URL travels with the model so late image loads cannot replace
/// a card whose episode changed while the same series stayed in its slot.
fn home_card_art_url(poster_url: &str, video: Option<&Video>, show_thumbnail: bool) -> String {
    video
        .filter(|_| show_thumbnail)
        .and_then(|v| v.thumbnail.as_deref())
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .unwrap_or(poster_url.trim())
        .to_string()
}

fn home_card_image(url: &str, fallback: &str) -> (Image, bool) {
    decoded_cache_get(&sized_cache_key(url, Some(DISPLAY_POSTER_SIDE)))
        .or_else(|| decoded_cache_get(&sized_cache_key(fallback, Some(DISPLAY_POSTER_SIDE))))
        .map(|buf| (Image::from_rgba8(buf), true))
        .unwrap_or_else(|| (Image::default(), false))
}

fn continue_remaining(record: Option<&EpisodeProgress>) -> String {
    let Some(record) = record else {
        return String::new();
    };
    if record.watched {
        return text::tr("Completed").into();
    }
    if record.position_secs > 0.0
        && record.duration_secs > record.position_secs
        && record.position_secs.is_finite()
        && record.duration_secs.is_finite()
    {
        return text::minutes_left(
            ((record.duration_secs - record.position_secs) / 60.0).ceil() as u64,
        );
    }
    String::new()
}

/// One Home → Upcoming display row: library + episode-cache joins for a
/// single [`UpcomingEntry`]. `index` is the entry's position in the full
/// Upcoming list, so picks from filtered views (the calendar's selected day)
/// resolve through [`Bridge::upcoming_picked`].
fn upcoming_row(
    entries: &[LibraryEntry],
    u: &UpcomingEntry,
    index: usize,
    date_relative: bool,
    show_thumbnail: bool,
) -> Option<UpcomingRow> {
    let e = entries.iter().find(|e| e.id == u.series_id)?;
    let episodes = read_episodes_cache_for(&u.type_, &u.series_id).unwrap_or_default();
    let v = episodes.iter().find(|v| v.id == u.episode_id)?;
    let subtitle = format!("{} · {}", episode_badge(v), episode_row_label(v));
    let date = v
        .released
        .as_deref()
        .and_then(|d| Bridge::format_human_date(d, date_relative))
        .unwrap_or_default();
    let art_url = home_card_art_url(&e.poster_url, Some(v), show_thumbnail);
    let (poster, is_loaded) = home_card_image(&art_url, &e.poster_url);
    Some(UpcomingRow {
        id: SharedString::from(&u.series_id),
        title: SharedString::from(&e.name),
        subtitle: SharedString::from(&subtitle),
        date: SharedString::from(&date),
        episode_title: episode_row_label(v).into(),
        ep_no: episode_badge(v).into(),
        art_url: art_url.into(),
        poster,
        is_loaded,
        index: index as i32,
    })
}

/// Gregorian (year, month 1–12, day) from days since the Unix epoch (Howard
/// Hinnant's `civil_from_days`). Inputs here are real air dates (≥ 1970), so
/// the era division stays non-negative.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Days since the Unix epoch from a Gregorian date (Hinnant's
/// `days_from_civil`). Inverse of [`civil_from_days`].
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = ((month + 9) % 12) as i64; // Mar = 0 … Feb = 11
    let doy = (153 * mp + 2) / 5 + day as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Epoch days of the 1st of `days`' month.
fn month_first(days: i64) -> i64 {
    let (year, month, _) = civil_from_days(days);
    days_from_civil(year, month, 1)
}

/// Weekday of an epoch day, Monday = 0 … Sunday = 6. 1970-01-01 was a
/// Thursday, hence the +3.
fn weekday_monday0(days: i64) -> u32 {
    (days + 3).rem_euclid(7) as u32
}

/// Keep the calendar on live data after the Upcoming list changes: with no
/// episodes the selection clears (the month stays for browsing); a stale
/// selection re-autos to today when it airs something, else the earliest air
/// date, and the month follows. A still-valid selection is left alone —
/// including its month, so browsing another month survives rebuilds.
fn heal_upcoming_cal(state: &mut Shared) {
    if state.upcoming_cal.first == 0 {
        state.upcoming_cal.first = month_first(today_days());
    }
    let valid = state
        .upcoming_cal
        .day
        .is_some_and(|d| state.upcoming_list.iter().any(|u| u.air_days == d));
    if valid {
        return;
    }
    let today = today_days();
    let today_live = state.upcoming_list.iter().any(|u| u.air_days == today);
    state.upcoming_cal.day = if state.upcoming_list.is_empty() {
        None
    } else if today_live {
        Some(today)
    } else {
        state.upcoming_list.iter().map(|u| u.air_days).min()
    };
    if let Some(day) = state.upcoming_cal.day {
        state.upcoming_cal.first = month_first(day);
    }
}

/// The 42 month-grid cells for the month starting at `first` (epoch days of
/// its 1st): full weeks starting Monday, filler days from the edge months
/// included so the grid never shifts shape. `counts` maps epoch days to
/// episode counts.
fn cal_cells(
    first: i64,
    counts: &HashMap<i64, usize>,
    selected: Option<i64>,
    today: i64,
) -> Vec<CalCell> {
    let (year, month, _) = civil_from_days(first);
    let start = first - weekday_monday0(first) as i64;
    (0..42)
        .map(|k| {
            let epoch = start + k;
            let (y, m, d) = civil_from_days(epoch);
            CalCell {
                epoch: epoch as i32,
                day: d as i32,
                in_month: y == year && m == month,
                count: counts.get(&epoch).copied().unwrap_or(0) as i32,
                selected: selected == Some(epoch),
                today: epoch == today,
            }
        })
        .collect()
}

/// Which badge a Continue Watching card gets: 0 = resume (no badge — the
/// progress rail shows it), 1 = "Next up" (still working through the
/// series: older dated, released episodes unwatched), 2 = "New Episode"
/// (everything else dated and released is already watched, so this one is
/// new). `started` is whether the offered episode has started, unwatched
/// progress; `episode_id` the offered episode; `is_watched` resolves any
/// episode against the progress map.
fn continue_badge(
    started: bool,
    episode_id: &str,
    episodes: &[Video],
    is_watched: impl Fn(&Video) -> bool,
) -> i32 {
    if started {
        return 0;
    }
    let backlog = episodes.iter().any(|v| {
        v.id != episode_id
            && episode_has_air_date(v)
            && episode_is_out(v, today_days())
            && !is_watched(v)
    });
    if backlog { 1 } else { 2 }
}

/// Which episode/movie id Continue Watching should offer for one library entry:
/// the in-progress one, or — once a series' latest episode is finished — the
/// next dated, released, not-yet-watched episode. Dateless episodes never
/// surface on their own (no schedule to count down to); a manually started
/// one still resumes through the in-progress branch. Movies have no "next",
/// so they only appear while in progress. `None` means nothing left to continue.
pub(crate) fn continue_resume_id(
    type_: &str,
    item_id: &str,
    latest: &EpisodeProgress,
    episodes: Option<&[Video]>,
    progress: &HashMap<String, EpisodeProgress>,
) -> Option<String> {
    if type_ == "movie" {
        return (!latest.watched && latest.position_secs > 0.0).then(|| latest.episode_id.clone());
    }
    if latest.watched {
        next_episode_to_watch(item_id, episodes?, progress).map(|v| v.id.clone())
    } else if latest.position_secs > 0.0 {
        Some(latest.episode_id.clone())
    } else {
        None
    }
}

/// Read the Continue Watching hide map (empty when absent/corrupt).
pub(crate) fn read_continue_hidden() -> HashMap<String, u64> {
    read_json::<HashMap<String, u64>>(CONTINUE_HIDDEN_KEY).unwrap_or_default()
}

/// Persist the Continue Watching hide map and, unless remote data is being
/// applied, push it mesh-wide (`continue_hidden` domain). Removed keys become
/// tombstones via `sync_records`.
pub(crate) fn write_continue_hidden(map: &HashMap<String, u64>) {
    write_json(CONTINUE_HIDDEN_KEY, map);
    if !applying() {
        notify_continue_hidden(map);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_card_artwork_respects_episode_availability_and_spoiler_preference() {
        let mut episode = Video {
            thumbnail: Some(" https://example.invalid/episode.jpg ".into()),
            ..Default::default()
        };
        let poster = "https://example.invalid/poster.jpg";
        assert_eq!(
            home_card_art_url(poster, Some(&episode), true),
            "https://example.invalid/episode.jpg"
        );
        assert_eq!(home_card_art_url(poster, Some(&episode), false), poster);
        assert_eq!(home_card_art_url(poster, None, true), poster);
        episode.thumbnail = Some("  ".into());
        assert_eq!(home_card_art_url(poster, Some(&episode), true), poster);
        episode.thumbnail = None;
        assert_eq!(home_card_art_url(poster, Some(&episode), true), poster);
    }

    #[test]
    fn home_card_keeps_cached_poster_until_episode_artwork_arrives() {
        use nova_media::cache::decoded_cache_insert;
        let primary = "https://example.invalid/home-card-cache-episode.jpg";
        let fallback = "https://example.invalid/home-card-cache-poster.jpg";
        decoded_cache_insert(
            &sized_cache_key(fallback, Some(DISPLAY_POSTER_SIDE)),
            SharedPixelBuffer::<Rgba8Pixel>::new(3, 2),
        );
        let (image, loaded) = home_card_image(primary, fallback);
        assert!(loaded);
        assert_eq!(image.size().width, 3);
        decoded_cache_insert(
            &sized_cache_key(primary, Some(DISPLAY_POSTER_SIDE)),
            SharedPixelBuffer::<Rgba8Pixel>::new(4, 2),
        );
        let (image, loaded) = home_card_image(primary, fallback);
        assert!(loaded);
        assert_eq!(image.size().width, 4);
    }

    #[test]
    fn home_remaining_time_requires_known_playback_and_rounds_up() {
        text::with_language(nova_config::Language::English, || {
            assert_eq!(continue_remaining(None), "");
            let mut record = EpisodeProgress {
                position_secs: 120.0,
                duration_secs: 241.0,
                ..Default::default()
            };
            assert_eq!(continue_remaining(Some(&record)), "3 min left");
            record.duration_secs = 121.0;
            assert_eq!(continue_remaining(Some(&record)), "1 min left");
            for duration in [0.0, 110.0, f64::NAN, f64::INFINITY] {
                record.duration_secs = duration;
                assert_eq!(continue_remaining(Some(&record)), "");
            }
            record.duration_secs = 240.0;
            record.position_secs = 0.0;
            assert_eq!(continue_remaining(Some(&record)), "");
            record.watched = true;
            assert_eq!(continue_remaining(Some(&record)), "Completed");
        });
        text::with_language(nova_config::Language::Croatian, || {
            assert_eq!(
                continue_remaining(Some(&EpisodeProgress {
                    position_secs: 120.0,
                    duration_secs: 241.0,
                    ..Default::default()
                })),
                "još 3 min"
            );
        });
    }

    fn showcase_source(catalog: &str, genre: &str) -> HomeCatalogSource {
        HomeCatalogSource {
            addon_url: "https://example.invalid/home-addon".into(),
            type_: "series".into(),
            catalog_id: catalog.into(),
            genre: genre.into(),
        }
    }

    fn showcase_preview(id: &str) -> MetaPreview {
        MetaPreview {
            id: id.into(),
            type_: "series".into(),
            name: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn featured_catalog_errors_are_distinct_from_successful_empty_catalogs() {
        for bytes in [
            br#"{}"#.as_slice(),
            br#"{"error":"upstream unavailable"}"#,
            br#"{"metas":null}"#,
            br#"{"metas":"invalid"}"#,
        ] {
            assert!(parse_home_showcase_catalog(bytes, "series").is_none());
        }
        assert!(
            parse_home_showcase_catalog(br#"{"metas":[]}"#, "series")
                .unwrap()
                .is_empty()
        );
        let previews = parse_home_showcase_catalog(
            br#"{"metas":[{"id":"tt1", "type":"", "name":"Title"}]}"#,
            "series",
        )
        .unwrap();
        assert_eq!(previews[0].type_, "series");
    }

    #[test]
    fn featured_cache_round_trips_full_selections_and_keeps_failed_catalogs() {
        let first = showcase_source("popular", "Action");
        let second = showcase_source("latest", "Comedy");
        let selected = vec![first.clone(), second.clone()];
        let mut cached = HomeShowcaseCache::default();
        let mut preview = showcase_preview("cached-a");
        preview.background = Some("https://images.example/a.jpg".into());
        preview.description = Some("Cached description".into());
        preview.runtime = Some("24 min".into());
        cached.update(
            &selected,
            vec![
                (first.clone(), Some(vec![preview])),
                (second.clone(), Some(vec![showcase_preview("cached-b")])),
            ],
        );
        let mut cached: HomeShowcaseCache =
            serde_json::from_str(&serde_json::to_string(&cached).unwrap()).unwrap();
        let before = cached.previews(&selected);
        assert_eq!(before[0].description.as_deref(), Some("Cached description"));
        assert_eq!(before[0].runtime.as_deref(), Some("24 min"));
        assert_eq!(
            before[0].background.as_deref(),
            Some("https://images.example/a.jpg")
        );
        cached.update(
            &selected,
            vec![
                (first.clone(), None),
                (second.clone(), Some(vec![showcase_preview("fresh-b")])),
            ],
        );
        assert_eq!(
            cached
                .previews(&selected)
                .iter()
                .map(|preview| preview.id.as_str())
                .collect::<Vec<_>>(),
            vec!["cached-a", "fresh-b"]
        );
        assert_eq!(
            cached.previews(&[second.clone(), first.clone()])[0].id,
            "fresh-b"
        );
        assert!(
            cached
                .previews(&[showcase_source("popular", "Comedy")])
                .is_empty()
        );
        let mut other_type = first.clone();
        other_type.type_ = "movie".into();
        assert!(cached.previews(&[other_type]).is_empty());
        let mut other_addon = first.clone();
        other_addon.addon_url = "https://other.example".into();
        assert!(cached.previews(&[other_addon]).is_empty());
        cached.update(
            std::slice::from_ref(&second),
            vec![(second.clone(), Some(Vec::new()))],
        );
        assert!(cached.previews(&selected).is_empty());
        assert_eq!(cached.catalogs.len(), 1, "removed selections are pruned");
    }

    #[test]
    fn featured_cache_keeps_enough_bounded_candidates_for_overlapping_catalogs() {
        let first = showcase_source("popular", "");
        let second = showcase_source("latest", "");
        let selected = vec![first.clone(), second.clone()];
        let mut cached = HomeShowcaseCache::default();
        let page = (0..100)
            .map(|index| showcase_preview(&format!("title-{index}")))
            .collect::<Vec<_>>();
        cached.update(
            &selected,
            vec![(first, Some(page.clone())), (second, Some(page))],
        );
        assert!(
            cached
                .catalogs
                .iter()
                .all(|catalog| catalog.previews.len() == 10)
        );
        let previews = cached.previews(&selected);
        assert_eq!(previews.len(), 10);
        assert_eq!(previews[5].id, "title-5");
        assert_eq!(previews[9].id, "title-9");
    }

    #[test]
    fn featured_list_reuses_artwork_by_url_and_anchors_navigation_by_identity() {
        let mut old = showcase_preview("old");
        old.background = Some("https://images.example/old.jpg".into());
        let pixels = SharedPixelBuffer::<Rgba8Pixel>::new(2, 2);
        let mut state = Shared {
            home_showcase: vec![old.clone(), showcase_preview("other")],
            home_showcase_displayed: Some(old.clone()),
            home_showcase_list_generation: 7,
            home_showcase_artwork: HashMap::from([(
                0,
                HomeShowcaseArtwork {
                    backdrop: Some(pixels),
                    backdrop_done: true,
                    backdrop_loading: false,
                },
            )]),
            ..Default::default()
        };
        let fresh = vec![showcase_preview("new"), old, showcase_preview("next")];
        assert_eq!(home_showcase_refreshed_index(&state, &fresh, 1), 2);
        assert_eq!(home_showcase_refreshed_index(&state, &fresh, -1), 0);
        replace_home_showcase_list(&mut state, fresh, 2);
        assert_eq!(state.home_showcase_list_generation, 8);
        assert!(state.home_showcase_artwork[&1].backdrop.is_some());
        assert_eq!(
            state.home_showcase_displayed.as_ref().unwrap().id,
            "old",
            "actions retain the visible preview until new artwork commits"
        );
        assert!(!state.home_showcase_artwork.contains_key(&0));
        assert_eq!(home_showcase_refreshed_index(&state, &[], 1), 0);
    }

    #[test]
    fn featured_cache_restores_before_network_and_refresh_waits_for_navigation() {
        // Isolate the process-wide Slint platform and KV store from other
        // app tests. Every live request is disabled in this fixture.
        const TEST_DIR: &str = "NOVA_HOME_SHOWCASE_CACHE_TEST_DIR";
        let Some(root) = std::env::var_os(TEST_DIR) else {
            let root = std::env::temp_dir().join(format!(
                "nova-home-cache-test-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap()).env(TEST_DIR, &root)
                .args(["--exact", "app::home::tests::featured_cache_restores_before_network_and_refresh_waits_for_navigation", "--nocapture"])
                .output().unwrap();
            fs::remove_dir_all(root).unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        storage::init_at(&root);
        let app = AppWindow::new().unwrap();
        app.window().set_size(slint::PhysicalSize::new(360, 800));
        app.window().show().unwrap();
        app.set_show_home(true);
        let player = crate::player::Player::setup(&app);
        let downloads = DownloadCoordinator::new(root.join("downloads"));
        #[cfg(feature = "desktop")]
        let bridge = {
            let (hi, _) = mpsc::channel();
            let (lo, _) = mpsc::channel();
            Bridge::new(
                app.as_weak(),
                PosterTx { hi, lo },
                Arc::new(Mutex::new(PosterStore::new(1))),
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
        let source = showcase_source("popular", "Action");
        {
            let mut state = bridge.shared.lock().unwrap();
            state.cache_settings.home_catalog_sources = vec![source.clone(), source.clone()];
            state.installed = vec![Installed {
                url:source.addon_url.clone(), label:"Catalog".into(), enabled:true,
                configure_ok:None, available:false, generation:1,
                manifest:serde_json::from_value(serde_json::json!({"id":"home-fixture", "name":"Fixture", "version":"1", "types":["series"], "resources":["catalog"], "catalogs":[{"type":"series", "id":"popular", "name":"Popular"}]})).unwrap()
            }];
            assert_eq!(home_showcase_sources(&state), vec![source.clone()]);
        }
        let mut cached = HomeShowcaseCache::default();
        let mut cached_first = showcase_preview("cached-a");
        cached_first.release_info = Some(serde_json::json!(2016));
        cached_first
            .extra
            .insert("tagline".into(), serde_json::json!("Cached tagline"));
        cached.update(
            std::slice::from_ref(&source),
            vec![(
                source.clone(),
                Some(vec![cached_first, showcase_preview("cached-b")]),
            )],
        );
        write_json(HOME_SHOWCASE_CACHE_KEY, &cached);
        bridge.refresh_home_showcase();
        assert_eq!(app.get_home_featured_title(), "cached-a");
        assert_eq!(app.get_home_featured_release_info(), "2016");
        assert_eq!(app.get_home_featured_tagline(), "Cached tagline");
        assert_eq!(app.get_home_featured_count(), 2);
        let revision = app.get_home_featured_revision();
        let generation = bridge.home_showcase_gen.load(Ordering::Relaxed);
        let old_art_generation = bridge.shared.lock().unwrap().home_showcase_list_generation;
        let fresh = vec![
            showcase_preview("fresh-c"),
            showcase_preview("cached-a"),
            showcase_preview("fresh-b"),
        ];
        bridge.finish_home_showcase_refresh(
            generation,
            vec![source.clone()],
            vec![(source.clone(), Some(fresh))],
        );
        assert_eq!(app.get_home_featured_title(), "cached-a");
        assert_eq!(app.get_home_featured_count(), 2);
        assert_eq!(
            app.get_home_featured_revision(),
            revision,
            "network completion must not trigger a crossfade"
        );
        assert!(app.get_home_featured_refresh_pending());
        let persisted = read_json::<HomeShowcaseCache>(HOME_SHOWCASE_CACHE_KEY).unwrap();
        assert_eq!(
            persisted.previews(std::slice::from_ref(&source))[0].id,
            "fresh-c"
        );
        bridge.home_showcase_step(1);
        assert_eq!(app.get_home_featured_title(), "fresh-b");
        assert_eq!(app.get_home_featured_release_info(), "");
        assert_eq!(app.get_home_featured_tagline(), "");
        assert_eq!(app.get_home_featured_index(), 2);
        assert_eq!(app.get_home_featured_count(), 3);
        assert!(!app.get_home_featured_refresh_pending());
        bridge.finish_home_showcase_artwork(0, old_art_generation, None);
        assert_eq!(
            app.get_home_featured_title(),
            "fresh-b",
            "old artwork must not select an index from the replacement list"
        );
        bridge.finish_home_showcase_refresh(
            generation.wrapping_sub(1),
            vec![source.clone()],
            vec![(source.clone(), Some(vec![showcase_preview("stale")]))],
        );
        assert!(
            !app.get_home_featured_refresh_pending(),
            "stale refreshes cannot replace the cache or list"
        );
        bridge.finish_home_showcase_refresh(
            generation,
            vec![source.clone()],
            vec![(source.clone(), None)],
        );
        assert!(
            !app.get_home_featured_refresh_pending(),
            "failed requests retain the successful cache"
        );

        // A single cached title has no regular paging timer, so a pending
        // refresh enables one deferred rotation boundary without changing it.
        bridge.install_home_showcase(vec![showcase_preview("single")]);
        let callback = bridge.clone();
        app.on_home_featured_step(move |delta| callback.home_showcase_step(delta));
        i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
        bridge.finish_home_showcase_refresh(
            generation,
            vec![source.clone()],
            vec![(
                source.clone(),
                Some(vec![
                    showcase_preview("single"),
                    showcase_preview("after-single"),
                ]),
            )],
        );
        i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_secs(8));
        assert_eq!(app.get_home_featured_title(), "single");
        assert_eq!(app.get_home_featured_count(), 1);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_secs(1));
        assert_eq!(app.get_home_featured_title(), "after-single");
        assert_eq!(app.get_home_featured_count(), 2);

        // Changed addon availability retries live fetching without blanking
        // the current slide; disabled addons do not restore stale selections.
        bridge.invalidate_home_showcase();
        bridge.ensure_home_showcase_loaded();
        assert_eq!(app.get_home_featured_title(), "after-single");
        bridge.shared.lock().unwrap().installed[0].enabled = false;
        bridge.invalidate_home_showcase();
        bridge.ensure_home_showcase_loaded();
        assert_eq!(app.get_home_featured_count(), 0);
        bridge.shared.lock().unwrap().installed[0].enabled = true;
        bridge.invalidate_home_showcase();
        bridge.ensure_home_showcase_loaded();
        assert_eq!(
            app.get_home_featured_title(),
            "single",
            "restoration rereads the latest persisted cache"
        );
        assert_eq!(app.get_home_featured_count(), 2);

        // While replacement art is pending, Details must still open the
        // displayed title, not whatever now occupies its numeric list index.
        {
            let mut state = bridge.shared.lock().unwrap();
            replace_home_showcase_list(&mut state, vec![showcase_preview("different")], 0);
        }
        bridge.home_showcase_picked();
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .id,
            "single"
        );
    }

    fn progress(
        series: &str,
        episode: &str,
        position: f64,
        watched: bool,
        updated: u64,
    ) -> EpisodeProgress {
        EpisodeProgress {
            series_id: series.to_string(),
            episode_id: episode.to_string(),
            position_secs: position,
            duration_secs: 3600.0,
            watched,
            unwatched_at_secs: 0,
            play_count: 1,
            updated_at_secs: updated,
        }
    }

    fn video(id: &str, season: u32, number: u32, released: Option<&str>) -> Video {
        Video {
            id: id.to_string(),
            name: id.to_string(),
            season: Some(season),
            episode: Some(number),
            released: released.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn movie_in_progress_offers_its_id() {
        let p = progress("m1", "m1", 600.0, false, 10);
        assert_eq!(
            continue_resume_id("movie", "m1", &p, None, &HashMap::new()),
            Some("m1".to_string())
        );
    }

    #[test]
    fn movie_watched_or_untouched_offers_nothing() {
        let watched = progress("m1", "m1", 3500.0, true, 10);
        assert_eq!(
            continue_resume_id("movie", "m1", &watched, None, &HashMap::new()),
            None
        );
        let untouched = progress("m1", "m1", 0.0, false, 10);
        assert_eq!(
            continue_resume_id("movie", "m1", &untouched, None, &HashMap::new()),
            None
        );
    }

    #[test]
    fn series_mid_episode_offers_it_and_finished_offers_next() {
        let eps = vec![
            video("s1:1:1", 1, 1, Some("2020-01-01")),
            video("s1:1:2", 1, 2, Some("2020-01-02")),
        ];
        let mut map = HashMap::new();
        let mid = progress("s1", "s1:1:1", 600.0, false, 10);
        map.insert(progress_map_key("s1", "s1:1:1"), mid.clone());
        assert_eq!(
            continue_resume_id("series", "s1", &mid, Some(&eps), &map),
            Some("s1:1:1".to_string())
        );
        let done = progress("s1", "s1:1:1", 3500.0, true, 11);
        map.insert(progress_map_key("s1", "s1:1:1"), done.clone());
        assert_eq!(
            continue_resume_id("series", "s1", &done, Some(&eps), &map),
            Some("s1:1:2".to_string())
        );
    }

    #[test]
    fn dateless_never_surfaces_unstarted_but_resumes_when_started() {
        let eps = vec![
            video("s1:1:1", 1, 1, Some("2020-01-01")),
            video("s1:1:2", 1, 2, None),
        ];
        // Finished the only dated episode: the dateless one is not offered
        // on its own (no air date to count down to).
        let mut map = HashMap::new();
        let done = progress("s1", "s1:1:1", 3500.0, true, 11);
        map.insert(progress_map_key("s1", "s1:1:1"), done.clone());
        assert_eq!(
            continue_resume_id("series", "s1", &done, Some(&eps), &map),
            None
        );
        // …but once the user starts the dateless episode itself, it resumes:
        // starting it was explicit, so it belongs in Continue Watching.
        let started = progress("s1", "s1:1:2", 600.0, false, 12);
        assert_eq!(
            continue_resume_id("series", "s1", &started, Some(&eps), &map),
            Some("s1:1:2".to_string())
        );
    }

    #[test]
    fn continue_badges_distinguish_next_up_from_new_episode() {
        let eps = vec![
            video("s1:1:1", 1, 1, Some("2020-01-01")),
            video("s1:1:2", 1, 2, Some("2020-01-02")),
            video("s1:1:3", 1, 3, Some("2020-01-03")),
        ];
        let watched = |v: &Video| ["s1:1:1", "s1:1:2"].contains(&v.id.as_str());
        // Unstarted offer with older episodes still unwatched → next up.
        assert_eq!(continue_badge(false, "s1:1:2", &eps, watched), 1);
        // Unstarted offer with everything else watched → new episode.
        let all_watched = |_: &Video| true;
        assert_eq!(continue_badge(false, "s1:1:3", &eps, all_watched), 2);
        // Started offer → resume, no badge either way.
        assert_eq!(continue_badge(true, "s1:1:2", &eps, watched), 0);
        assert_eq!(continue_badge(true, "s1:1:3", &eps, all_watched), 0);
    }

    #[test]
    fn series_finished_all_offers_nothing_and_needs_episodes() {
        let eps = vec![video("s1:1:1", 1, 1, Some("2020-01-01"))];
        let done = progress("s1", "s1:1:1", 3500.0, true, 11);
        let mut map = HashMap::new();
        map.insert(progress_map_key("s1", "s1:1:1"), done.clone());
        // All released episodes watched: nothing left to continue.
        assert_eq!(
            continue_resume_id("series", "s1", &done, Some(&eps), &map),
            None
        );
        // No episode list cached yet: cannot offer the next episode.
        assert_eq!(continue_resume_id("series", "s1", &done, None, &map), None);
    }

    #[test]
    fn civil_date_math_round_trips_known_dates() {
        // Epoch anchor: 1970-01-01 was a Thursday.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(weekday_monday0(0), 3);
        // Leap day, month ends, year rollover.
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(civil_from_days(days_from_civil(2026, 9, 27)), (2026, 9, 27));
        assert_eq!(
            civil_from_days(days_from_civil(1999, 12, 31)),
            (1999, 12, 31)
        );
        assert_eq!(civil_from_days(days_from_civil(2000, 1, 1)), (2000, 1, 1));
        // A known Monday: 2026-09-28.
        assert_eq!(weekday_monday0(days_from_civil(2026, 9, 28)), 0);
        assert_eq!(weekday_monday0(days_from_civil(2026, 10, 4)), 6);
        // Month starts land on the 1st.
        let sept = days_from_civil(2026, 9, 1);
        assert_eq!(month_first(days_from_civil(2026, 9, 27)), sept);
        assert_eq!(month_first(days_from_civil(2026, 9, 1)), sept);
    }

    #[test]
    fn calendar_heal_keeps_valid_month_and_repairs_stale_days() {
        fn state_with(days: &[i64]) -> Shared {
            Shared {
                upcoming_list: days
                    .iter()
                    .enumerate()
                    .map(|(i, d)| UpcomingEntry {
                        series_id: format!("s{i}"),
                        type_: "series".to_string(),
                        episode_id: format!("e{i}"),
                        air_days: *d,
                    })
                    .collect(),
                ..Shared::default()
            }
        }
        let today = today_days();
        let day_a = today + 3;
        let day_b = today + 10;

        // Empty list: selection clears, month starts at this month.
        let mut s = state_with(&[]);
        heal_upcoming_cal(&mut s);
        assert_eq!(s.upcoming_cal.day, None);
        assert_eq!(s.upcoming_cal.first, month_first(today));

        // Stale selection with today airing: today wins, month follows.
        let mut s = state_with(&[today, day_b]);
        s.upcoming_cal.day = Some(12345);
        heal_upcoming_cal(&mut s);
        assert_eq!(s.upcoming_cal.day, Some(today));
        assert_eq!(s.upcoming_cal.first, month_first(today));

        // Stale selection, today quiet: earliest air date wins.
        let mut s = state_with(&[day_a, day_b]);
        s.upcoming_cal.day = Some(12345);
        heal_upcoming_cal(&mut s);
        assert_eq!(s.upcoming_cal.day, Some(day_a));
        assert_eq!(s.upcoming_cal.first, month_first(day_a));

        // Valid selection in a browsed month: left alone, month included.
        let mut s = state_with(&[day_a, day_b]);
        let browsed = month_first(day_b + 60);
        s.upcoming_cal.day = Some(day_a);
        s.upcoming_cal.first = browsed;
        heal_upcoming_cal(&mut s);
        assert_eq!(s.upcoming_cal.day, Some(day_a));
        assert_eq!(s.upcoming_cal.first, browsed);
    }

    #[test]
    fn calendar_cells_cover_full_monday_weeks() {
        // September 2026: the 1st is a Tuesday, so the grid starts Monday
        // 2026-08-31 and runs 42 days.
        let first = days_from_civil(2026, 9, 1);
        let ep = days_from_civil(2026, 9, 27);
        let mut counts = HashMap::new();
        counts.insert(ep, 2usize);
        let cells = cal_cells(first, &counts, Some(ep), days_from_civil(2026, 9, 15));
        assert_eq!(cells.len(), 42);
        assert_eq!(cells[0].epoch as i64, days_from_civil(2026, 8, 31));
        assert_eq!((cells[0].day, cells[0].in_month), (31, false));
        assert_eq!((cells[1].day, cells[1].in_month), (1, true));
        // Weeks start on Monday.
        assert_eq!(weekday_monday0(cells[0].epoch as i64), 0);
        assert_eq!(weekday_monday0(cells[6].epoch as i64), 6);
        // Counts, selection and today land on the right cells.
        let marked = cells.iter().find(|c| c.epoch as i64 == ep).unwrap();
        assert_eq!((marked.count, marked.selected), (2, true));
        assert_eq!(cells.iter().filter(|c| c.today).count(), 1);
        assert!(cells.iter().filter(|c| c.in_month).count() == 30);
    }

    #[test]
    fn home_showcase_merges_catalogs_in_order_with_five_distinct_titles_each() {
        fn preview(id: &str, type_: &str) -> MetaPreview {
            MetaPreview {
                id: id.into(),
                type_: type_.into(),
                name: id.into(),
                ..MetaPreview::default()
            }
        }

        let first = (b'a'..=b'g')
            .map(|id| preview(&(id as char).to_string(), "series"))
            .collect();
        let second = ["a", "b", "f", "g", "h", "i", "j", "k"]
            .into_iter()
            .map(|id| preview(id, "series"))
            .collect();
        // Same id with a different protocol type is a different metadata item.
        let third = vec![preview("a", "movie")];
        let merged = merge_home_showcase_results(vec![(2, third), (1, second), (0, first)]);

        assert_eq!(merged.len(), 11);
        assert_eq!(merged[0].id, "a");
        assert_eq!(merged[4].id, "e");
        assert_eq!(merged[5].id, "f");
        assert_eq!(merged[9].id, "j");
        assert_eq!(
            (merged[10].id.as_str(), merged[10].type_.as_str()),
            ("a", "movie")
        );
    }
}

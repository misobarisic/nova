//! Home page: featured catalogs, Continue Watching, Watch history, and Upcoming.
use super::*;

const HOME_SHOWCASE_PER_CATALOG: usize = 5;
const HOME_RAW_CANDIDATES_PER_CATALOG: usize = 120;
const HOME_SHOWCASE_CACHE_KEY: &str = "home:showcase:v1";
const HOME_CATALOG_ROWS_PER_CATALOG: usize = 20;
const HOME_CATALOG_ROWS_CACHE_KEY: &str = "home:catalog-rows:v1";
const HOME_SHOWCASE_RETRY_DELAY: Duration = Duration::from_secs(30);
pub(super) type HomeShowcaseBatch = (HomeCatalogSource, Option<Vec<MetaPreview>>);

fn home_grouping_key(source: &HomeCatalogSource) -> nova_config::CatalogGroupingKey {
    nova_config::CatalogGroupingKey {
        addon_url: source.addon_url.clone(),
        type_: source.type_.clone(),
        catalog_id: source.catalog_id.clone(),
        surface: nova_config::CatalogSurface::Home,
    }
}

pub(super) fn catalog_grouping_context_sources(
    state: &Shared,
    key: &nova_config::CatalogGroupingKey,
    fingerprint: &str,
) -> Vec<HomeCatalogSource> {
    let mut seen = HashSet::new();
    [
        &state.catalog_grouping.home_featured_raw,
        &state.catalog_grouping.home_rows_raw,
    ]
    .into_iter()
    .flatten()
    .flat_map(|cache| &cache.catalogs)
    .filter(|catalog| {
        home_grouping_key(&catalog.source) == *key
            && catalog_grouping::raw_fingerprint(&catalog.previews) == fingerprint
            && seen.insert(catalog.source.clone())
    })
    .map(|catalog| catalog.source.clone())
    .collect()
}

pub(super) fn catalog_grouping_sources_selected(
    state: &Shared,
    key: &nova_config::CatalogGroupingKey,
    sources: &[HomeCatalogSource],
) -> bool {
    let selected = home_showcase_sources(state)
        .into_iter()
        .chain(home_catalog_row_sources(state));
    selected.into_iter().any(|source| {
        home_grouping_key(&source) == *key && (sources.is_empty() || sources.contains(&source))
    })
}

/// Device-local catalog results, keyed by the complete configured selection.
/// Failed requests keep the previous batch; only successful responses replace
/// it. Artwork uses the ordinary image cache rather than a second disk copy.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct HomeShowcaseCache {
    catalogs: Vec<HomeShowcaseCatalog>,
}

#[derive(Clone, Serialize, Deserialize)]
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
                    let mut previews = cached.previews.clone();
                    for preview in &mut previews {
                        preview.extra.insert(
                            "novaSourceUrl".into(),
                            serde_json::Value::String(source.addon_url.clone()),
                        );
                    }
                    Some((index, previews))
                })
                .collect(),
        )
    }

    fn update(&mut self, selected: &[HomeCatalogSource], batches: Vec<HomeShowcaseBatch>) {
        self.update_with_limit(selected, batches, HOME_RAW_CANDIDATES_PER_CATALOG);
    }

    fn update_with_limit(
        &mut self,
        selected: &[HomeCatalogSource],
        batches: Vec<HomeShowcaseBatch>,
        per_catalog: usize,
    ) {
        self.catalogs
            .retain(|cached| selected.contains(&cached.source));
        // Keep enough candidates to fill each selected rail after
        // deduplication, without persisting entire addon catalog pages.
        let limit = per_catalog.min(HOME_RAW_CANDIDATES_PER_CATALOG);
        for (source, previews) in batches {
            let Some(previews) = previews.filter(|_| selected.contains(&source)) else {
                continue;
            };
            let mut seen = HashSet::new();
            let previous = self.catalogs.iter().find(|cached| cached.source == source);
            let previews = previews
                .into_iter()
                .filter(|preview| {
                    !preview.id.is_empty()
                        && !preview.title().trim().is_empty()
                        && seen.insert((preview.type_.clone(), preview.id.clone()))
                })
                .take(limit)
                .map(|mut preview| {
                    if let Some(old) = previous.and_then(|cached| {
                        cached
                            .previews
                            .iter()
                            .find(|old| old.id == preview.id && old.type_ == preview.type_)
                    }) {
                        let fresh = preview;
                        preview = old.clone();
                        merge_home_showcase_preview(&mut preview, &fresh);
                    }
                    preview.extra.insert(
                        "novaSourceUrl".into(),
                        serde_json::Value::String(source.addon_url.clone()),
                    );
                    preview
                })
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

    fn enrich(&mut self, requested: &MetaPreview, preview: &MetaPreview) {
        for catalog in &mut self.catalogs {
            if nova_providers::ExternalId::parse(&requested.id).is_none()
                && requested
                    .extra
                    .get("novaSourceUrl")
                    .and_then(serde_json::Value::as_str)
                    != Some(catalog.source.addon_url.as_str())
            {
                continue;
            }
            for cached in &mut catalog.previews {
                if cached.id == preview.id && cached.type_ == preview.type_ {
                    merge_home_showcase_preview(cached, preview);
                }
            }
        }
    }
}

struct ContinueRowDraft {
    id: String,
    type_: String,
    episode_id: String,
    title: String,
    subtitle: String,
    episode_title: String,
    ep_no: String,
    remaining: String,
    art_url: String,
    poster_url: String,
    progress: f32,
    badge: i32,
}

/// A queued Home projection may outlive a synced/manual completion. Prune
/// against live progress before exposing rows, keeping row/action indexes equal.
fn prune_watched_new_episode_rows(state: &mut Shared) {
    let Shared {
        entries,
        progress,
        new_episodes_list,
        ..
    } = state;
    new_episodes_list.retain(|row| {
        entries.iter().any(|entry| {
            entry.id == row.series_id
                && entry.type_ == row.type_
                && entry.watch_status == WatchStatus::Auto
        }) && !progress
            .get(&progress_map_key(&row.series_id, &row.episode_id))
            .is_some_and(|record| record.watched)
    });
}

struct UpcomingRowDraft {
    id: String,
    title: String,
    subtitle: String,
    date: String,
    episode_title: String,
    ep_no: String,
    art_url: String,
    poster_url: String,
    index: usize,
}

#[derive(Clone)]
struct HomeLibraryIdentity {
    index: usize,
    type_: String,
    id: String,
}

/// A sparse catalog or failed optional enrichment must not erase known fields.
/// Supplied nonempty values still replace old values on a successful refresh.
fn merge_home_showcase_preview(preview: &mut MetaPreview, fresh: &MetaPreview) {
    if !fresh.name.trim().is_empty() {
        preview.name.clone_from(&fresh.name);
    }
    for (field, value) in [
        (&mut preview.title_legacy, &fresh.title_legacy),
        (&mut preview.poster, &fresh.poster),
        (&mut preview.background, &fresh.background),
        (&mut preview.description, &fresh.description),
        (&mut preview.runtime, &fresh.runtime),
        (&mut preview.logo, &fresh.logo),
    ] {
        if value
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            field.clone_from(value);
        }
    }
    if !fresh.genres.is_empty() {
        preview.genres.clone_from(&fresh.genres);
    }
    for (field, value) in [
        (&mut preview.release_info, &fresh.release_info),
        (&mut preview.imdb_rating, &fresh.imdb_rating),
    ] {
        if value.as_ref().is_some_and(usable_home_metadata_value) {
            field.clone_from(value);
        }
    }
    for (key, value) in &fresh.extra {
        // A detail response may have been supplied by another metadata addon;
        // the catalog's ownership remains the authority for private IDs.
        if usable_home_metadata_value(value)
            && (key != "novaSourceUrl" || !preview.extra.contains_key(key))
            && !matches!(
                key.as_str(),
                "novaCatalogDisplayKey" | "novaCatalogDisplaySeason" | "novaCatalogVirtualSeason"
            )
        {
            preview.extra.insert(key.clone(), value.clone());
        }
    }
    if let Some(season) = catalog_grouping::canonical_season(preview)
        && !fresh.title().trim().is_empty()
        && catalog_grouping::canonical_season(fresh) != Some(season)
    {
        preview.name = format!("{} · {} {season}", fresh.title(), text::tr("Season"));
        preview.title_legacy = None;
    }
}

fn usable_home_metadata_value(value: &serde_json::Value) -> bool {
    !value.is_null() && value.as_str().is_none_or(|value| !value.trim().is_empty())
}

fn home_showcase_art_urls(preview: &MetaPreview) -> Vec<String> {
    let mut urls = Vec::new();
    for url in [&preview.background, &preview.poster].into_iter().flatten() {
        let url = url.trim();
        if !url.is_empty() && !urls.iter().any(|existing| existing == url) {
            urls.push(url.to_owned());
        }
    }
    urls
}

fn home_showcase_backdrop_image(
    candidate: Option<SharedPixelBuffer<Rgba8Pixel>>,
    same_slide: bool,
    current: Image,
) -> Image {
    candidate.map(Image::from_rgba8).unwrap_or_else(|| {
        if same_slide {
            current
        } else {
            Image::default()
        }
    })
}

fn home_showcase_meta_urls(state: &Shared, preview: &MetaPreview) -> Vec<String> {
    let owner = preview
        .extra
        .get("novaSourceUrl")
        .and_then(serde_json::Value::as_str);
    let global_id = nova_providers::ExternalId::parse(&preview.id).is_some();
    let mut candidates = state
        .installed
        .iter()
        .filter(|addon| {
            addon.enabled && addon.available
            && addon.manifest.accepts("meta", &preview.type_, &preview.id)
            // Opaque catalog IDs belong to their originating provider. A
            // wildcard manifest is not evidence of shared identity.
            && (global_id || owner == Some(addon.url.as_str()))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|addon| owner != Some(addon.url.as_str()));
    candidates
        .into_iter()
        .take(4)
        .filter_map(|installed| {
            Addon::new(&installed.url)
                .ok()
                .map(|addon| addon.meta_url(&preview.type_, &preview.id))
        })
        .collect()
}

fn home_showcase_needs_metadata(preview: &MetaPreview) -> bool {
    [&preview.background, &preview.description, &preview.logo]
        .into_iter()
        .any(|value| value.as_deref().is_none_or(|value| value.trim().is_empty()))
}

/// UI-owned logo keys include the navigation target and the exact processing
/// identity. An inactive prefetch never parks its completion in the window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct HomeLogoIdentity {
    index: usize,
    generation: u64,
    media_type: String,
    media_id: String,
    source_url: String,
    url: String,
    recipe_key: String,
    revision: u64,
}
impl HomeLogoIdentity {
    fn for_preview(preview: &MetaPreview, index: usize, generation: u64) -> Option<Self> {
        let url = preview
            .logo
            .as_deref()
            .filter(|url| !url.trim().is_empty())?
            .to_owned();
        Some(Self {
            index,
            generation,
            media_type: preview.type_.clone(),
            media_id: preview.id.clone(),
            source_url: preview
                .extra
                .get("novaSourceUrl")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .into(),
            recipe_key: image_cache_key(&url, ImageUse::Fixed(None)),
            revision: image_settings_revision(),
            url,
        })
    }
    fn key(&self) -> SharedString {
        serde_json::to_string(self)
            .expect("Home logo identity")
            .into()
    }
}
fn clear_home_pending_logo(app: &AppWindow) {
    app.set_home_pending_logo(Image::default());
    app.set_home_pending_logo_identity(SharedString::default());
}

struct HomeBackdropRequest {
    index: usize,
    generation: u64,
    image_revision: u64,
    url: String,
    preferred_url: String,
}

fn home_showcase_same_identity(left: &MetaPreview, right: &MetaPreview) -> bool {
    left.id == right.id
        && left.type_ == right.type_
        && catalog_grouping::virtual_season(left) == catalog_grouping::virtual_season(right)
        && (nova_providers::ExternalId::parse(&left.id).is_some()
            || left.extra.get("novaSourceUrl") == right.extra.get("novaSourceUrl"))
}

fn restore_home_showcase_header(preview: &mut MetaPreview) {
    let Some(header) = read_meta_header_for(&preview.type_, &preview.id) else {
        return;
    };
    let fresh = preview.clone();
    preview.background = Some(header.background_url);
    preview.logo = Some(header.logo_url);
    preview.description = Some(header.description);
    preview.genres = header.genres;
    if !header.year.is_empty() {
        preview.release_info = Some(serde_json::Value::String(header.year));
    }
    // The latest catalog's supplied values take precedence over header caches.
    merge_home_showcase_preview(preview, &fresh);
}

impl HomeShowcaseArtwork {
    fn request(
        &mut self,
        preview: &MetaPreview,
        now: std::time::Instant,
    ) -> (Option<String>, Option<SharedPixelBuffer<Rgba8Pixel>>, bool) {
        self.request_with(preview, now, |url| {
            decoded_cache_get(&image_cache_key(url, ImageUse::Fixed(None)))
        })
    }

    fn request_with(
        &mut self,
        preview: &MetaPreview,
        now: std::time::Instant,
        mut cached: impl FnMut(&str) -> Option<SharedPixelBuffer<Rgba8Pixel>>,
    ) -> (Option<String>, Option<SharedPixelBuffer<Rgba8Pixel>>, bool) {
        if self.loading_url.is_some() {
            return (None, None, false);
        }
        let urls = home_showcase_art_urls(preview);
        for url in urls {
            // A prior successful fetch may have been evicted. Check the shared
            // cache on every visit before treating this URL as ready.
            if let Some(pixels) = cached(&url) {
                self.backdrop_url = url;
                return (None, Some(pixels), true);
            }
            if self
                .failed_urls
                .get(&url)
                .is_some_and(|at| now.saturating_duration_since(*at) < HOME_SHOWCASE_RETRY_DELAY)
            {
                continue;
            }
            self.loading_url = Some(url.clone());
            return (Some(url), None, false);
        }
        // No available image must never stall the carousel. A later visit or
        // rotation can retry failed URLs once their cooldown has elapsed.
        (None, None, true)
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

/// All selected poster-rail sources, including unavailable addons so their
/// local cached results remain visible while a manifest cannot be fetched.
fn home_catalog_row_sources(state: &Shared) -> Vec<HomeCatalogSource> {
    state
        .cache_settings
        .home_addon_sources(true)
        .into_iter()
        .filter(|source| {
            state
                .installed
                .iter()
                .any(|addon| addon.url == source.addon_url && addon.enabled)
        })
        .collect()
}

fn home_catalog_row_groups(
    cache: &HomeShowcaseCache,
    sources: &[HomeCatalogSource],
    installed: &[Installed],
) -> Vec<HomeCatalogGroup> {
    sources
        .iter()
        .filter_map(|source| {
            let cached = cache
                .catalogs
                .iter()
                .find(|catalog| &catalog.source == source)?;
            let addon = installed.iter().find(|addon| addon.url == source.addon_url);
            let title = addon
                .and_then(|addon| {
                    addon
                        .manifest
                        .catalog_for(&source.type_, &source.catalog_id)
                })
                .map(|catalog| catalog.name.clone())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| source.catalog_id.clone());
            let previews = cached
                .previews
                .iter()
                .take(HOME_CATALOG_ROWS_PER_CATALOG)
                .cloned()
                .map(|mut preview| {
                    preview.extra.insert(
                        "novaSourceUrl".into(),
                        serde_json::Value::String(source.addon_url.clone()),
                    );
                    restore_home_showcase_header(&mut preview);
                    if let Some(header) = read_meta_header_for(&preview.type_, &preview.id)
                        && !header.poster_url.trim().is_empty()
                    {
                        preview.poster = Some(header.poster_url);
                    }
                    preview
                })
                .collect();
            Some(HomeCatalogGroup {
                source: HomeRowSource::Addon(source.clone()),
                title,
                previews,
            })
        })
        .collect()
}

pub(super) fn parse_home_showcase_catalog(bytes: &[u8], type_: &str) -> Option<Vec<MetaPreview>> {
    // A 200 response carrying an addon error or an empty stub is a failed
    // refresh, rather than proof that a previously cached catalog is empty.
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value.get("metas")?.as_array()?;
    let mut previews = Addon::parse_catalog(bytes).ok()?;
    for preview in &mut previews {
        preview.extra.remove(similar::RECOMMENDATION_SOURCE);
        if preview.type_.is_empty() {
            preview.type_ = type_.into();
        }
    }
    Some(previews)
}

fn home_catalog_targets(
    sources: &[HomeCatalogSource],
    installed: &[Installed],
) -> Vec<(HomeCatalogSource, String)> {
    sources
        .iter()
        .filter_map(|source| {
            let addon = installed
                .iter()
                .find(|addon| addon.url == source.addon_url && addon.enabled && addon.available)?;
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
        .collect()
}

// Share transport and response collection, while each Home surface retains
// its own generation checks, cache limits and publication policy.
impl Bridge {
    fn schedule_home_grouping_candidates(
        &self,
        key: nova_config::CatalogGroupingKey,
        raw: &[MetaPreview],
        projected: &[MetaPreview],
        visible_limit: usize,
    ) {
        if self
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .catalog_grouping_mode(&key)
            == nova_config::CatalogGroupingMode::Original
        {
            return;
        }
        let visible = projected
            .iter()
            .take(visible_limit)
            .flat_map(|preview| {
                self.catalog_grouping_display_context(preview)
                    .map(|context| context.members)
                    .unwrap_or_else(|| {
                        vec![nova_providers::catalog_grouping::SourceIdentity {
                            addon_url: key.addon_url.clone(),
                            media_type: preview.type_.clone(),
                            id: preview.id.clone(),
                        }]
                    })
            })
            .collect::<HashSet<_>>();
        let candidates = raw
            .iter()
            .filter(|preview| {
                visible.contains(&nova_providers::catalog_grouping::SourceIdentity {
                    addon_url: key.addon_url.clone(),
                    media_type: preview.type_.clone(),
                    id: preview.id.clone(),
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        self.schedule_catalog_grouping(key, &candidates);
    }

    fn project_home_showcase_cache(
        &self,
        cache: &HomeShowcaseCache,
        sources: &[HomeCatalogSource],
    ) -> Vec<MetaPreview> {
        self.shared
            .lock()
            .unwrap()
            .catalog_grouping
            .home_featured_raw = Some(cache.clone());
        let mut projected = cache.clone();
        for catalog in &mut projected.catalogs {
            if !sources.contains(&catalog.source) {
                continue;
            }
            let key = home_grouping_key(&catalog.source);
            let raw = &catalog.previews;
            let previews = self.project_catalog_grouping(&key, raw);
            self.schedule_home_grouping_candidates(key, raw, &previews, HOME_SHOWCASE_PER_CATALOG);
            catalog.previews = previews;
        }
        projected
            .previews(sources)
            .into_iter()
            .filter(|m| {
                profile_policy::allows_title(&profiles::ProfileStore::capture(), &m.type_, &m.id)
            })
            .collect()
    }

    fn project_home_row_cache(
        &self,
        cache: &HomeShowcaseCache,
        sources: &[HomeCatalogSource],
        installed: &[Installed],
    ) -> Vec<HomeCatalogGroup> {
        self.shared.lock().unwrap().catalog_grouping.home_rows_raw = Some(cache.clone());
        let mut projected = cache.clone();
        for catalog in &mut projected.catalogs {
            if !sources.contains(&catalog.source) {
                continue;
            }
            let key = home_grouping_key(&catalog.source);
            let raw = &catalog.previews;
            let previews = self.project_catalog_grouping(&key, raw);
            self.schedule_home_grouping_candidates(
                key,
                raw,
                &previews,
                HOME_CATALOG_ROWS_PER_CATALOG,
            );
            catalog.previews = previews;
        }
        home_catalog_row_groups(&projected, sources, installed)
    }

    /// Reproject the in-memory raw Home batches. Settings changes and accepted
    /// background evidence must not trigger another catalog HTTP request.
    pub(super) fn reproject_home_catalog_grouping(&self) {
        let (featured, rows, featured_sources, row_sources, installed) = {
            let state = self.shared.lock().unwrap();
            (
                state.catalog_grouping.home_featured_raw.clone(),
                state.catalog_grouping.home_rows_raw.clone(),
                home_showcase_sources(&state),
                home_catalog_row_sources(&state),
                state.installed.clone(),
            )
        };
        if let Some(cache) = featured {
            self.install_home_showcase(self.project_home_showcase_cache(&cache, &featured_sources));
        }
        if let Some(cache) = rows {
            self.install_prepared_home_catalog_rows(self.project_home_row_cache(
                &cache,
                &row_sources,
                &installed,
            ));
        }
    }

    /// Load a small first page from each selected Home catalog. Each catalog
    /// contributes at most five distinct titles; Discover's current picker,
    /// pagination and search state are intentionally untouched.
    pub(super) fn refresh_home_showcase(&self) {
        if profile_policy::restricted() {
            self.install_home_showcase(Vec::new());
            return;
        }
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
            let targets = home_catalog_targets(&sources, &state.installed);
            (
                state.cache_settings.home_catalog_sources.clone(),
                sources,
                targets,
                restore,
            )
        };
        if sources.is_empty() {
            self.install_home_showcase(Vec::new());
        }
        let bridge = self.clone();
        thread::spawn(move || {
            let mut cached =
                read_json::<HomeShowcaseCache>(HOME_SHOWCASE_CACHE_KEY).unwrap_or_default();
            cached.update(&selected, Vec::new());
            let _ = profiles::invoke_from_event_loop(move || {
                if bridge.home_showcase_gen.load(Ordering::Relaxed) != generation
                    || home_showcase_sources(&bridge.shared.lock().unwrap()) != sources
                {
                    return;
                }
                if restore {
                    bridge.install_home_showcase(
                        bridge.project_home_showcase_cache(&cached, &sources),
                    );
                }
                let finish = bridge.clone();
                home_catalog_fetch::fetch(
                    &bridge,
                    targets,
                    HOME_SHOWCASE_PER_CATALOG,
                    home_catalog_fetch::Scope::Showcase(generation),
                    move |batches| {
                        finish.finish_home_showcase_refresh(generation, sources, batches);
                    },
                );
            });
        });
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
        } else {
            let (index, generation) = {
                let state = self.shared.lock().unwrap();
                (
                    state.home_showcase_index,
                    state.home_showcase_list_generation,
                )
            };
            self.ensure_home_showcase_art(index, generation);
        }
    }

    /// Restore cached poster rails immediately, then refresh each configured
    /// addon catalog in the background. This cache is independent of the
    /// rotating banner because the rows can contain larger first pages.
    pub(super) fn refresh_home_catalog_rows(&self) {
        if profile_policy::restricted() {
            self.install_prepared_home_catalog_rows(Vec::new());
            return;
        }
        let generation = self.home_catalog_rows_gen.fetch_add(1, Ordering::Relaxed) + 1;
        let (selected, sources, targets, restore, installed) = {
            let mut state = self.shared.lock().unwrap();
            let was_loaded = state.home_catalog_rows_loaded;
            state.home_catalog_rows_loaded = true;
            let sources = home_catalog_row_sources(&state);
            let restore = !was_loaded
                || state.home_catalog_row_sources != sources
                || (state.home_catalog_row_groups.is_empty() && !sources.is_empty());
            if restore {
                state.home_catalog_row_sources = sources.clone();
            }
            let targets = home_catalog_targets(&sources, &state.installed);
            (
                state.cache_settings.home_addon_sources(false),
                sources,
                targets,
                restore,
                state.installed.clone(),
            )
        };

        if sources.is_empty() {
            self.install_prepared_home_catalog_rows(Vec::new());
        }
        let bridge = self.clone();
        thread::spawn(move || {
            let mut cached =
                read_json::<HomeShowcaseCache>(HOME_CATALOG_ROWS_CACHE_KEY).unwrap_or_default();
            cached.update_with_limit(&selected, Vec::new(), HOME_RAW_CANDIDATES_PER_CATALOG);
            let _ = profiles::invoke_from_event_loop(move || {
                if bridge.home_catalog_rows_gen.load(Ordering::Relaxed) != generation
                    || home_catalog_row_sources(&bridge.shared.lock().unwrap()) != sources
                {
                    return;
                }
                if restore || sources.is_empty() {
                    let groups = bridge.project_home_row_cache(&cached, &sources, &installed);
                    bridge.install_prepared_home_catalog_rows(groups);
                }
                let finish = bridge.clone();
                home_catalog_fetch::fetch(
                    &bridge,
                    targets,
                    HOME_CATALOG_ROWS_PER_CATALOG,
                    home_catalog_fetch::Scope::Rows(generation),
                    move |batches| {
                        finish.finish_home_catalog_rows_refresh(generation, sources, batches);
                    },
                );
            });
        });
    }

    pub(super) fn invalidate_home_catalog_rows(&self) {
        self.home_catalog_rows_gen.fetch_add(1, Ordering::Relaxed);
        let mut state = self.shared.lock().unwrap();
        state.home_catalog_rows_loaded = false;
    }

    pub(super) fn ensure_home_catalog_rows_loaded(&self) {
        let refresh = {
            let state = self.shared.lock().unwrap();
            !state.home_catalog_rows_loaded
                || home_catalog_row_sources(&state) != state.home_catalog_row_sources
        };
        if refresh {
            self.refresh_home_catalog_rows();
        } else {
            self.dispatch_home_catalog_posters();
        }
    }

    fn finish_home_catalog_rows_refresh(
        &self,
        generation: u64,
        sources: Vec<HomeCatalogSource>,
        batches: Vec<HomeShowcaseBatch>,
    ) {
        if generation != self.home_catalog_rows_gen.load(Ordering::Relaxed) {
            return;
        }
        let (selected, installed) = {
            let state = self.shared.lock().unwrap();
            if home_catalog_row_sources(&state) != sources {
                return;
            }
            (
                state.cache_settings.home_addon_sources(false),
                state.installed.clone(),
            )
        };
        let mut cached =
            read_json::<HomeShowcaseCache>(HOME_CATALOG_ROWS_CACHE_KEY).unwrap_or_default();
        cached.update_with_limit(&selected, batches, HOME_RAW_CANDIDATES_PER_CATALOG);
        write_json(HOME_CATALOG_ROWS_CACHE_KEY, &cached);
        self.install_prepared_home_catalog_rows(
            self.project_home_row_cache(&cached, &sources, &installed),
        );
        self.request_progress_projection();
    }

    pub(super) fn install_home_catalog_rows(&self, groups: Vec<HomeCatalogGroup>) {
        if self.app().is_none() {
            return;
        }
        self.rebuild_watch_history_list();
        self.install_prepared_home_catalog_rows(groups);
    }

    /// Progress workers already joined history against metadata. Keep this
    /// publication path free of the cache reads in rebuild_watch_history_list.
    pub(super) fn install_prepared_home_catalog_rows(&self, mut groups: Vec<HomeCatalogGroup>) {
        for group in &mut groups {
            group.previews.retain(|m| {
                profile_policy::allows_title(&profiles::ProfileStore::capture(), &m.type_, &m.id)
            });
        }
        let Some(app) = self.app() else { return };
        let view = app.get_home_view();
        let history_was_open = view >= 3
            && (view - 3) as usize == self.shared.lock().unwrap().home_catalog_row_groups.len();
        let open_source = if view >= 3 {
            self.shared
                .lock()
                .unwrap()
                .home_catalog_row_groups
                .get((view - 3) as usize)
                .map(|group| group.source.clone())
        } else {
            None
        };
        // Recommendation results are independent of addon catalog refreshes;
        // always inject the current profile's filtered projection.
        groups.retain(|group| group.source != HomeRowSource::BecauseYouWatched);
        if let Some(group) = self.recommendation_group() {
            groups.push(group);
        }
        // Keep live decoded images when language/progress changes republish
        // models after their original cache entries have been evicted.
        let previous_cards: HashMap<_, _> = app
            .get_home_catalog_cards()
            .iter()
            .chain(app.get_home_history_archive_cards().iter())
            .filter(|card| card.is_loaded)
            .map(|card| {
                (
                    (
                        card.media_type.to_string(),
                        card.id.to_string(),
                        card.poster_url.to_string(),
                    ),
                    card.poster,
                )
            })
            .collect();
        let mut sections = Vec::new();
        let mut scroll_keys = Vec::new();
        let mut cards = Vec::new();
        let mut items = Vec::new();
        let groups = groups
            .into_iter()
            .filter(|group| !group.previews.is_empty())
            .collect::<Vec<_>>();
        for group in &groups {
            scroll_keys.push(home_catalog_scroll_key(&group.source));
            let section_index = sections.len() as i32;
            sections.push(HomeCatalogSection {
                title: SharedString::from(&group.title),
                first_card: cards.len() as i32,
                card_count: group.previews.len() as i32,
                // The group has already passed typed exclusions and MAX_CARDS.
                tmdb_attribution: group.source == HomeRowSource::BecauseYouWatched
                    && similar::tmdb_attribution(&group.previews),
                is_history: false,
            });
            for preview in &group.previews {
                let flat_index = items.len();
                let poster_url = preview.poster.as_deref().unwrap_or_default();
                let pixels = (!poster_url.is_empty())
                    .then(|| {
                        decoded_cache_get(&image_cache_key(
                            poster_url,
                            ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
                        ))
                    })
                    .flatten();
                let previous = previous_cards.get(&(
                    preview.type_.clone(),
                    preview.id.clone(),
                    poster_url.to_owned(),
                ));
                cards.push(HomeCatalogCard {
                    recommendation: group.source == HomeRowSource::BecauseYouWatched,
                    section_index,
                    flat_index: flat_index as i32,
                    id: SharedString::from(&preview.id),
                    media_type: SharedString::from(&preview.type_),
                    title: SharedString::from(preview.title()),
                    year: SharedString::from(preview.year_str().unwrap_or_default()),
                    history_action: SharedString::default(),
                    history_date: SharedString::default(),
                    progress: 0.0,
                    poster_url: SharedString::from(poster_url),
                    poster: pixels
                        .as_ref()
                        .map(|pixels| Image::from_rgba8(pixels.clone()))
                        .or_else(|| previous.cloned())
                        .unwrap_or_default(),
                    is_loaded: pixels.is_some() || previous.is_some(),
                });
                items.push(preview.clone());
            }
        }
        let history = self.shared.lock().unwrap().home_history_list.clone();
        if !history.is_empty() {
            scroll_keys.push(home_catalog_scroll_key(&HomeRowSource::WatchHistory));
            let section_index = sections.len() as i32;
            sections.push(HomeCatalogSection {
                title: text::tr("Watch history").into(),
                first_card: cards.len() as i32,
                card_count: history.len() as i32,
                tmdb_attribution: false,
                is_history: true,
            });
            for (index, entry) in history.iter().enumerate() {
                let pixels = (!entry.poster_url.is_empty())
                    .then(|| {
                        decoded_cache_get(&image_cache_key(
                            &entry.poster_url,
                            ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
                        ))
                    })
                    .flatten();
                let previous = previous_cards.get(&(
                    entry.type_.clone(),
                    entry.series_id.clone(),
                    entry.poster_url.clone(),
                ));
                cards.push(HomeCatalogCard {
                    recommendation: false,
                    section_index,
                    flat_index: (items.len() + index) as i32,
                    id: entry.series_id.clone().into(),
                    media_type: entry.type_.clone().into(),
                    title: entry.title.clone().into(),
                    year: entry.caption().into(),
                    history_date: SharedString::default(),
                    history_action: entry.action_label().into(),
                    progress: if entry.can_resume() {
                        progress_fraction(entry.position_secs, entry.duration_secs)
                    } else {
                        0.0
                    },
                    poster_url: entry.poster_url.clone().into(),
                    poster: pixels
                        .as_ref()
                        .map(|pixels| Image::from_rgba8(pixels.clone()))
                        .or_else(|| previous.cloned())
                        .unwrap_or_default(),
                    is_loaded: pixels.is_some() || previous.is_some(),
                });
            }
        }
        // A settings reorder must keep an open grid tied to its source,
        // rather than silently displaying the new catalog at the old index.
        let next_view = if history_was_open && !history.is_empty() {
            groups.len() as i32 + 3
        } else {
            open_source
                .as_ref()
                .and_then(|source| groups.iter().position(|group| &group.source == source))
                .map_or(0, |index| index as i32 + 3)
        };
        {
            let mut state = self.shared.lock().unwrap();
            state.home_catalog_row_groups = groups;
            state.home_catalog_row_items = items;
        }
        app.set_home_catalog_scroll_keys(Rc::new(VecModel::from(scroll_keys)).into());
        app.set_home_catalog_sections(Rc::new(VecModel::from(sections)).into());
        app.set_home_catalog_cards(Rc::new(VecModel::from(cards)).into());
        let archive = self.shared.lock().unwrap().home_history_archive.clone();
        let archive_cards = archive
            .iter()
            .map(|entry| {
                let pixels = decoded_cache_get(&image_cache_key(
                    &entry.poster_url,
                    ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
                ));
                let previous = previous_cards.get(&(
                    entry.type_.clone(),
                    entry.series_id.clone(),
                    entry.poster_url.clone(),
                ));
                let (year, month, day) = civil_from_days((entry.played_at_secs / 86400) as i64);
                let date = text::absolute_date(day, month, year);
                let caption = entry.caption();
                HomeCatalogCard {
                    id: entry.series_id.clone().into(),
                    media_type: entry.type_.clone().into(),
                    title: entry.title.clone().into(),
                    year: caption.into(),
                    history_date: date.into(),
                    history_action: entry.action_label().into(),
                    progress: if entry.can_resume() {
                        progress_fraction(entry.position_secs, entry.duration_secs)
                    } else {
                        0.0
                    },
                    poster_url: entry.poster_url.clone().into(),
                    poster: pixels
                        .as_ref()
                        .map(|pixels| Image::from_rgba8(pixels.clone()))
                        .or_else(|| previous.cloned())
                        .unwrap_or_default(),
                    is_loaded: pixels.is_some() || previous.is_some(),
                    ..Default::default()
                }
            })
            .collect::<Vec<_>>();
        app.set_home_history_archive_cards(Rc::new(VecModel::from(archive_cards)).into());
        self.publish_home_catalog_order();
        if view >= 3 {
            app.set_home_view(next_view);
        }
        self.dispatch_home_catalog_posters();
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
        let previews = self.project_home_showcase_cache(&cached, &sources);
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
                        .and_then(|preview| home_showcase_art_urls(preview).into_iter().next());
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
                net::fetch_image(url, ImageUse::Fixed(None), |_| {});
            }
        }
    }

    fn install_home_showcase(&self, mut previews: Vec<MetaPreview>) {
        previews.retain(|m| {
            profile_policy::allows_title(&profiles::ProfileStore::capture(), &m.type_, &m.id)
        });
        let anchor = self.shared.lock().unwrap().home_showcase_displayed.clone();
        let index = anchor
            .as_ref()
            .and_then(|anchor| {
                previews.iter().position(|preview| {
                    home_showcase_same_identity(preview, anchor)
                        || self
                            .catalog_grouping_display_context(preview)
                            .is_some_and(|context| {
                                context.members.iter().any(|member| {
                                    member.id == anchor.id
                                        && member.media_type == anchor.type_
                                        && anchor
                                            .extra
                                            .get("novaSourceUrl")
                                            .and_then(serde_json::Value::as_str)
                                            == Some(member.addon_url.as_str())
                                }) && (context.canonical_season.is_none()
                                    || context.canonical_season
                                        == catalog_grouping::canonical_season(anchor))
                            })
                })
            })
            .unwrap_or(0);
        let (first, count) = {
            let mut state = self.shared.lock().unwrap();
            replace_home_showcase_list(&mut state, previews, index);
            state.home_showcase_pending_index = (!state.home_showcase.is_empty()).then_some(index);
            state.home_showcase_displayed = state.home_showcase.get(index).cloned();
            (
                state.home_showcase.get(index).cloned(),
                state.home_showcase.len(),
            )
        };
        if let Some(app) = self.app() {
            clear_home_pending_logo(&app);
            app.set_home_featured_logo_identity(SharedString::default());
        }
        self.refresh_home_watch_action();
        let Some(first) = first else {
            if let Some(app) = self.app() {
                app.set_home_featured_refresh_pending(false);
                app.set_home_featured_title(SharedString::default());
                app.set_home_featured_logo(Image::default());
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
            app.set_home_featured_logo(
                transparent_title_logo(first.logo.as_deref().and_then(|url| {
                    decoded_cache_get(&image_cache_key(url, ImageUse::Fixed(None)))
                }))
                .map(Image::from_rgba8)
                .unwrap_or_default(),
            );
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
            let backdrop = home_showcase_art_urls(&first)
                .iter()
                .find_map(|url| decoded_cache_get(&image_cache_key(url, ImageUse::Fixed(None))));
            app.set_home_featured_backdrop(backdrop.map(Image::from_rgba8).unwrap_or_default());
            app.set_home_featured_index(index as i32);
            app.set_home_featured_count(count as i32);
            app.set_home_featured_revision(app.get_home_featured_revision().wrapping_add(1));
        }
        let generation = self.shared.lock().unwrap().home_showcase_list_generation;
        self.ensure_home_showcase_art(index, generation);
    }

    /// Catalog previews are intentionally sparse. Hydrate only the current
    /// and prefetched slide through the same metadata path as Detail, without
    /// blocking native artwork or depending on Discover's prefetch preference.
    fn ensure_home_showcase_metadata(&self, index: usize, generation: u64) {
        let revision = nova_providers::metadata_revision();
        let request = {
            let mut state = self.shared.lock().unwrap();
            if generation != state.home_showcase_list_generation {
                return;
            }
            let Some(preview) = state.home_showcase.get(index).cloned() else {
                return;
            };
            if !home_showcase_needs_metadata(&preview)
                && (preview.type_ == "movie"
                    || read_episodes_cache_for(&preview.type_, &preview.id)
                        .is_some_and(|videos| !videos.is_empty()))
            {
                return;
            }
            if state
                .home_showcase_artwork
                .values()
                .filter(|artwork| artwork.metadata_loading)
                .count()
                >= 2
            {
                return;
            }
            let urls = home_showcase_meta_urls(&state, &preview);
            let artwork = state.home_showcase_artwork.entry(index).or_default();
            if artwork.metadata_loading
                || (artwork.metadata_revision == Some(revision)
                    && artwork
                        .metadata_retry_at
                        .is_none_or(|at| at.elapsed() < HOME_SHOWCASE_RETRY_DELAY))
            {
                return;
            }
            artwork.metadata_revision = Some(revision);
            artwork.metadata_retry_at = None;
            if urls.is_empty() {
                return;
            }
            artwork.metadata_loading = true;
            (preview, urls.into())
        };
        self.fetch_home_showcase_metadata(index, generation, revision, request.0, request.1);
    }

    fn fetch_home_showcase_metadata(
        &self,
        index: usize,
        generation: u64,
        revision: u64,
        preview: MetaPreview,
        mut urls: VecDeque<String>,
    ) {
        let Some(url) = urls.pop_front() else {
            return;
        };
        let bridge = self.clone();
        net::fetch_bytes(url, move |result| {
            let item = result
                .ok()
                .and_then(|bytes| Addon::parse_meta(&bytes).ok().flatten())
                .filter(|item| meta_matches_request(item, &preview.type_, &preview.id));
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                bridge.finish_home_showcase_metadata(
                    index, generation, revision, preview, urls, item,
                );
            });
        });
    }

    fn finish_home_showcase_metadata(
        &self,
        index: usize,
        generation: u64,
        revision: u64,
        requested: MetaPreview,
        urls: VecDeque<String>,
        item: Option<MetaItem>,
    ) {
        {
            let mut state = self.shared.lock().unwrap();
            if generation != state.home_showcase_list_generation
                || state
                    .home_showcase
                    .get(index)
                    .is_none_or(|preview| !home_showcase_same_identity(preview, &requested))
            {
                return;
            }
            let artwork = state.home_showcase_artwork.entry(index).or_default();
            if revision != nova_providers::metadata_revision() {
                artwork.metadata_loading = false;
                artwork.metadata_revision = None;
                drop(state);
                self.ensure_home_showcase_metadata(index, generation);
                return;
            }
            if item.is_none() && !urls.is_empty() {
                drop(state);
                self.fetch_home_showcase_metadata(index, generation, revision, requested, urls);
                return;
            }
            artwork.metadata_loading = false;
            artwork.metadata_retry_at = item
                .as_ref()
                .is_none_or(|item| home_showcase_needs_metadata(&item.preview))
                .then(std::time::Instant::now);
            if let Some(item) = &item {
                if let Some(cache) = &mut state.catalog_grouping.home_featured_raw {
                    cache.enrich(&requested, &item.preview);
                }
                let previous = serde_json::to_value(&state.home_showcase[index]).ok();
                merge_home_showcase_preview(&mut state.home_showcase[index], &item.preview);
                let changed = previous != serde_json::to_value(&state.home_showcase[index]).ok();
                if let Some(refresh) = &mut state.home_showcase_refresh {
                    for preview in refresh {
                        if home_showcase_same_identity(preview, &requested) {
                            merge_home_showcase_preview(preview, &item.preview);
                        }
                    }
                }
                if changed
                    && state.home_showcase_index == index
                    && state.home_showcase_pending_index.is_none()
                {
                    state.home_showcase_pending_index = Some(index);
                }
            }
        }
        if let Some(item) = item {
            // Share successful metadata with Detail/Library and keep the richer
            // Home snapshot across restarts and later sparse catalog refreshes.
            let header = meta_header_from_item(&item);
            merge_meta_header_for(&requested.type_, &requested.id, &header);
            if !item.videos.is_empty() {
                write_episode_meta_cache_for(&requested.type_, &requested.id, &item);
            }
            let mut cached =
                read_json::<HomeShowcaseCache>(HOME_SHOWCASE_CACHE_KEY).unwrap_or_default();
            cached.enrich(&requested, &item.preview);
            write_json(HOME_SHOWCASE_CACHE_KEY, &cached);
        }
        self.refresh_home_watch_action();
        self.ensure_home_showcase_art(index, generation);
        let (current, count) = {
            let state = self.shared.lock().unwrap();
            (
                state
                    .home_showcase_pending_index
                    .unwrap_or(state.home_showcase_index),
                state.home_showcase.len(),
            )
        };
        if count > 0 {
            self.ensure_home_showcase_metadata(current, generation);
            self.ensure_home_showcase_metadata((current + 1) % count, generation);
        }
    }

    /// Begin loading the requested slide's art, while keeping the currently
    /// displayed slide intact. The next slide is prefetched after every commit.
    fn ensure_home_showcase_art(&self, index: usize, generation: u64) {
        self.ensure_home_showcase_metadata(index, generation);
        self.ensure_home_showcase_logo(index, generation);
        let (fetch, backdrop, ready) = {
            let mut state = self.shared.lock().unwrap();
            if generation != state.home_showcase_list_generation {
                return;
            }
            let Some(preview) = state.home_showcase.get(index).cloned() else {
                return;
            };
            let artwork = state.home_showcase_artwork.entry(index).or_default();
            artwork.request(&preview, std::time::Instant::now())
        };

        if ready {
            let should_commit =
                self.shared.lock().unwrap().home_showcase_pending_index == Some(index);
            if should_commit {
                self.commit_home_showcase_index(index, generation, backdrop);
            }
        }
        if let Some(url) = fetch {
            self.fetch_home_showcase_artwork(index, generation, url);
        }
    }

    /// Settings/list/target changes can invalidate parked pixels even while
    /// Home is covered. This only releases UI ownership; it starts no work.
    pub(super) fn invalidate_home_pending_logo(&self) {
        let Some(app) = self.app() else { return };
        let target_key = {
            let state = self.shared.lock().unwrap();
            state
                .home_showcase_pending_index
                .and_then(|target| {
                    state.home_showcase.get(target).and_then(|preview| {
                        HomeLogoIdentity::for_preview(
                            preview,
                            target,
                            state.home_showcase_list_generation,
                        )
                    })
                })
                .map(|identity| identity.key())
                .unwrap_or_default()
        };
        if app.get_home_pending_logo_identity() != target_key {
            clear_home_pending_logo(&app);
        }
    }

    /// Optional logo fetches never delay navigation or backdrop readiness.
    pub(super) fn ensure_home_showcase_logo(&self, index: usize, generation: u64) {
        let Some(app) = self.app() else { return };
        self.invalidate_home_pending_logo();
        let identity = {
            let mut state = self.shared.lock().unwrap();
            if generation != state.home_showcase_list_generation {
                return;
            }
            let Some(preview) = state.home_showcase.get(index).cloned() else {
                return;
            };
            let displayed = state.home_showcase_index == index
                && state
                    .home_showcase_displayed
                    .as_ref()
                    .is_some_and(|shown| home_showcase_same_identity(shown, &preview));
            let pending = state.home_showcase_pending_index == Some(index);
            let Some(identity) = HomeLogoIdentity::for_preview(&preview, index, generation) else {
                if displayed {
                    app.set_home_featured_logo(Image::default());
                    app.set_home_featured_logo_identity(SharedString::default());
                }
                return;
            };
            let key = identity.key();
            // The window's current or target logo is already a successful
            // delivery. Cache eviction must not trigger another download.
            if (displayed
                && app.get_home_featured_logo_identity() == key
                && app.get_home_featured_logo().size().width > 0)
                || (pending
                    && app.get_home_pending_logo_identity() == key
                    && app.get_home_pending_logo().size().width > 0)
            {
                return;
            }
            let artwork = state.home_showcase_artwork.entry(index).or_default();
            if artwork.logo_url != identity.url {
                artwork.logo_url = identity.url.clone();
                artwork.logo_loading = false;
                artwork.logo_failed_at = None;
            }
            if let Some(pixels) = transparent_title_logo(decoded_cache_get(&identity.recipe_key)) {
                let image = Image::from_rgba8(pixels);
                if displayed {
                    app.set_home_featured_logo(image.clone());
                    app.set_home_featured_logo_identity(key.clone());
                    app.set_home_featured_revision(
                        app.get_home_featured_revision().wrapping_add(1),
                    );
                }
                if pending {
                    app.set_home_pending_logo(image);
                    app.set_home_pending_logo_identity(key);
                }
                return;
            }
            if artwork.logo_loading
                || artwork
                    .logo_failed_at
                    .is_some_and(|at| at.elapsed() < HOME_SHOWCASE_RETRY_DELAY)
            {
                return;
            }
            artwork.logo_loading = true;
            identity
        };
        let bridge = self.clone();
        net::fetch_image(identity.url.clone(), ImageUse::Fixed(None), move |pixels| {
            let pixels = transparent_title_logo(pixels);
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                let Some(app) = bridge.app() else { return };
                let mut state = bridge.shared.lock().unwrap();
                if state.home_showcase_list_generation != identity.generation {
                    return;
                }
                let current = state.home_showcase.get(identity.index).cloned();
                let exact = current
                    .as_ref()
                    .and_then(|preview| {
                        HomeLogoIdentity::for_preview(preview, identity.index, identity.generation)
                    })
                    .as_ref()
                    == Some(&identity);
                let artwork = state
                    .home_showcase_artwork
                    .entry(identity.index)
                    .or_default();
                if artwork.logo_url != identity.url {
                    return;
                }
                artwork.logo_loading = false;
                if !exact {
                    let target = state
                        .home_showcase_pending_index
                        .unwrap_or(state.home_showcase_index);
                    let retry = target == identity.index;
                    drop(state);
                    bridge.invalidate_home_pending_logo();
                    if retry {
                        bridge.ensure_home_showcase_logo(identity.index, identity.generation);
                    }
                    return;
                }
                artwork.logo_failed_at = pixels.is_none().then(std::time::Instant::now);
                let displayed = state.home_showcase_index == identity.index
                    && state
                        .home_showcase_displayed
                        .as_ref()
                        .zip(current.as_ref())
                        .is_some_and(|(shown, preview)| {
                            home_showcase_same_identity(shown, preview)
                        });
                let pending = state.home_showcase_pending_index == Some(identity.index);
                drop(state);
                let key = identity.key();
                if displayed {
                    app.set_home_featured_logo(
                        pixels.clone().map(Image::from_rgba8).unwrap_or_default(),
                    );
                    app.set_home_featured_logo_identity(if pixels.is_some() {
                        key.clone()
                    } else {
                        SharedString::default()
                    });
                    app.set_home_featured_revision(
                        app.get_home_featured_revision().wrapping_add(1),
                    );
                }
                if pending {
                    app.set_home_pending_logo(pixels.map(Image::from_rgba8).unwrap_or_default());
                    app.set_home_pending_logo_identity(
                        if app.get_home_pending_logo().size().width > 0 {
                            key
                        } else {
                            SharedString::default()
                        },
                    );
                }
            });
        });
    }

    fn fetch_home_showcase_artwork(&self, index: usize, generation: u64, url: String) {
        let bridge = self.clone();
        let preferred_url = self
            .shared
            .lock()
            .unwrap()
            .home_showcase
            .get(index)
            .and_then(|preview| home_showcase_art_urls(preview).into_iter().next())
            .unwrap_or_else(|| url.clone());
        let request = HomeBackdropRequest {
            index,
            generation,
            image_revision: image_settings_revision(),
            url: url.clone(),
            preferred_url,
        };
        // Keep the add-on-provided source resolution for the large hero rather
        // than using the smaller detail-banner derivative.
        net::fetch_image(url.clone(), ImageUse::Fixed(None), move |pixels| {
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                bridge.finish_home_showcase_artwork_request(request, pixels);
            });
        });
    }

    #[cfg(test)]
    pub(super) fn finish_home_showcase_artwork(
        &self,
        index: usize,
        generation: u64,
        image_revision: u64,
        url: String,
        pixels: Option<SharedPixelBuffer<Rgba8Pixel>>,
    ) {
        self.finish_home_showcase_artwork_request(
            HomeBackdropRequest {
                index,
                generation,
                image_revision,
                preferred_url: url.clone(),
                url,
            },
            pixels,
        );
    }

    fn finish_home_showcase_artwork_request(
        &self,
        request: HomeBackdropRequest,
        pixels: Option<SharedPixelBuffer<Rgba8Pixel>>,
    ) {
        let HomeBackdropRequest {
            index,
            generation,
            image_revision,
            url,
            preferred_url,
        } = request;
        let (valid_url, should_commit, preferred_changed) = {
            let mut state = self.shared.lock().unwrap();
            if generation != state.home_showcase_list_generation {
                return;
            }
            let Some(preview) = state.home_showcase.get(index) else {
                return;
            };
            let urls = home_showcase_art_urls(preview);
            let valid_url = urls.contains(&url);
            let preferred_changed = urls
                .first()
                .is_some_and(|current| current != &preferred_url);
            let Some(artwork) = state.home_showcase_artwork.get_mut(&index) else {
                return;
            };
            if artwork.loading_url.as_deref() != Some(&url) {
                return;
            }
            if image_revision != image_settings_revision() {
                artwork.loading_url = None;
                drop(state);
                self.ensure_home_showcase_art(index, generation);
                return;
            }
            artwork.loading_url = None;
            if valid_url {
                if pixels.is_some() {
                    artwork.backdrop_url = url.clone();
                    artwork.failed_urls.remove(&url);
                } else {
                    artwork.failed_urls.insert(url, std::time::Instant::now());
                }
            }
            if state.home_showcase_index == index && state.home_showcase_pending_index.is_none() {
                state.home_showcase_pending_index = Some(index);
            }
            (
                valid_url,
                state.home_showcase_pending_index == Some(index),
                preferred_changed,
            )
        };
        if valid_url && pixels.is_some() {
            // A successful image need not fit the LRU, and another worker may
            // evict it before this callback runs. Hand its pixels straight to
            // the visible slide; inactive prefetches keep only their key.
            if should_commit {
                self.commit_home_showcase_index(index, generation, pixels);
                // Metadata can add or replace the preferred background while
                // a poster fallback is loading. Keep its delivered pixels
                // visible, then pursue only that changed preference; ordinary
                // failed-background fallbacks must not restart a zero-LRU loop.
                if preferred_changed {
                    self.shared.lock().unwrap().home_showcase_pending_index = Some(index);
                    self.ensure_home_showcase_art(index, generation);
                }
            }
        } else {
            self.ensure_home_showcase_art(index, generation);
        }
    }

    pub(super) fn commit_home_showcase_index(
        &self,
        index: usize,
        generation: u64,
        backdrop: Option<SharedPixelBuffer<Rgba8Pixel>>,
    ) {
        let (preview, count, same_slide) = {
            let mut state = self.shared.lock().unwrap();
            if generation != state.home_showcase_list_generation {
                return;
            }
            if state.home_showcase_pending_index != Some(index) {
                return;
            }
            if !state.home_showcase_artwork.contains_key(&index) {
                return;
            }
            let Some(preview) = state.home_showcase.get(index).cloned() else {
                return;
            };
            let count = state.home_showcase.len();
            let same_slide = state
                .home_showcase_displayed
                .as_ref()
                .is_some_and(|displayed| home_showcase_same_identity(displayed, &preview));
            state.home_showcase_index = index;
            state.home_showcase_pending_index = None;
            state.home_showcase_displayed = Some(preview.clone());
            (preview, count, same_slide)
        };

        self.refresh_home_watch_action();

        if let Some(app) = self.app() {
            app.set_home_featured_title(SharedString::from(preview.title()));
            let identity = HomeLogoIdentity::for_preview(&preview, index, generation);
            let key = identity
                .as_ref()
                .map(|identity| identity.key())
                .unwrap_or_default();
            let logo = if !key.is_empty() && app.get_home_pending_logo_identity() == key {
                app.get_home_pending_logo()
            } else if let Some(pixels) = identity
                .as_ref()
                .and_then(|identity| decoded_cache_get(&identity.recipe_key))
                .and_then(|pixels| transparent_title_logo(Some(pixels)))
            {
                Image::from_rgba8(pixels)
            } else if same_slide && !key.is_empty() && app.get_home_featured_logo_identity() == key
            {
                app.get_home_featured_logo()
            } else {
                Image::default()
            };
            app.set_home_featured_logo_identity(if logo.size().width > 0 {
                key
            } else {
                SharedString::default()
            });
            app.set_home_featured_logo(logo);
            clear_home_pending_logo(&app);
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
            let image = home_showcase_backdrop_image(
                backdrop,
                same_slide,
                app.get_home_featured_backdrop(),
            );
            app.set_home_featured_backdrop(image);
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
            clear_home_pending_logo(&app);
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

    pub(super) fn refresh_home_watch_action(&self) {
        let preview = self.shared.lock().unwrap().home_showcase_displayed.clone();
        let label = if let Some(preview) = preview {
            let videos = if preview.type_ == "movie" {
                None
            } else {
                read_episodes_cache_for(&preview.type_, &preview.id)
            };
            let state = self.shared.lock().unwrap();
            watch_action_label(&preview.id, videos.as_deref(), &state.progress)
        } else {
            text::tr("Start").into()
        };
        if let Some(app) = self.app() {
            app.set_home_featured_watch_label(label.into());
        }
    }

    pub(super) fn home_catalog_card_picked(&self, index: usize) {
        let (preview, addon_count) = {
            let state = self.shared.lock().unwrap();
            (
                state.home_catalog_row_items.get(index).cloned(),
                state.home_catalog_row_items.len(),
            )
        };
        if let Some(preview) = preview {
            self.open_preview(preview, 0, false, None, false);
        } else if index >= addon_count {
            self.watch_history_picked(index - addon_count);
        }
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
        self.migrate_viewing_activity();
        let groups = self.library_display_groups();
        // Capture typed positions and the latest record per library identity.
        // The history map stays borrowed while indexing; only the selected
        // latest records are copied for the later cache pass.
        let (identities, latest) = {
            let state = self.shared.lock().unwrap();
            let mut latest_refs: HashMap<&str, &EpisodeProgress> = HashMap::new();
            for record in state.progress.values() {
                if latest_refs
                    .get(record.series_id.as_str())
                    .is_none_or(|previous| {
                        continue_playback_order(record) > continue_playback_order(previous)
                    })
                {
                    latest_refs.insert(&record.series_id, record);
                }
            }
            let identities = state
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    profile_policy::allows_title(
                        &profiles::ProfileStore::capture(),
                        &e.type_,
                        &e.id,
                    ) && e.watch_status == WatchStatus::Auto
                        && latest_refs.contains_key(e.id.as_str())
                        && groups.is_representative(&e.type_, &e.id)
                })
                .map(|(index, entry)| HomeLibraryIdentity {
                    index,
                    type_: entry.type_.clone(),
                    id: entry.id.clone(),
                })
                .collect::<Vec<_>>();
            let latest: HashMap<String, EpisodeProgress> = identities
                .iter()
                .filter_map(|identity| {
                    let record = latest_refs.get(identity.id.as_str())?;
                    Some((identity.id.clone(), (**record).clone()))
                })
                .collect::<HashMap<_, _>>();
            (identities, latest)
        };
        let activity = read_viewing_activity();
        let new_hidden: HashMap<String, Vec<String>> =
            read_json(NEW_EPISODES_HIDDEN_KEY).unwrap_or_default();
        let today = today_days();
        let mut new_episodes: Vec<(i64, ContinueEntry)> = Vec::new();
        let mut list: Vec<ContinueEntry> = Vec::new();
        let mut episode_identities = HashMap::new();
        for identity in identities {
            if !profile_policy::allows_title(
                &profiles::ProfileStore::capture(),
                &identity.type_,
                &identity.id,
            ) {
                continue;
            }
            // Process each cache independently so a large metadata response
            // never overlaps the next series' Video allocation.
            let episodes = (identity.type_ != "movie")
                .then(|| read_episodes_cache_for(&identity.type_, &identity.id))
                .flatten();
            let state = self.shared.lock().unwrap();
            let Some(entry) = state.entries.get(identity.index).filter(|entry| {
                entry.id == identity.id
                    && entry.type_ == identity.type_
                    && entry.watch_status == WatchStatus::Auto
            }) else {
                continue;
            };
            let Some(p) = latest.get(&identity.id) else {
                continue;
            };
            let episode_slice = episodes.as_deref().unwrap_or_default();
            let bucket = activity_bucket(
                &entry.id,
                episode_slice,
                &state.progress,
                activity.get(&entry.id),
                today,
            );
            if bucket == "Completed" {
                continue;
            }
            let returning =
                entry.type_ != "movie" && bucket == "New episodes" && !genuine_partial_rewatch(p);
            let episode_id = if returning {
                next_episode_to_watch(&entry.id, episode_slice, &state.progress)
                    .map(|v| v.id.clone())
            } else {
                continue_resume_id(
                    &entry.type_,
                    &entry.id,
                    p,
                    episodes.as_deref(),
                    &state.progress,
                )
            };
            let Some(episode_id) = episode_id else {
                continue;
            };
            let proofs = HomeEpisodeProofs::new(&entry.type_, episode_slice);
            let identity = proofs.aliases(&entry.type_, &episode_id);
            if returning {
                let available = new_episode_ids(&entry.id, episode_slice, &state.progress);
                if new_episodes_are_hidden(
                    &new_hidden,
                    &entry.type_,
                    &entry.id,
                    &available,
                    &proofs,
                ) {
                    continue;
                }
            } else if continue_is_hidden(
                &state.continue_hidden,
                &entry.type_,
                &entry.id,
                &identity,
                p.updated_at_secs,
            ) {
                continue;
            }
            episode_identities.insert(
                (entry.type_.clone(), entry.id.clone(), episode_id.clone()),
                identity,
            );
            let card = ContinueEntry {
                series_id: entry.id.clone(),
                type_: entry.type_.clone(),
                episode_id,
                updated_at_secs: p.updated_at_secs,
            };
            if returning {
                let newest = episode_slice
                    .iter()
                    .filter(|v| {
                        episode_has_air_date(v)
                            && episode_is_out(v, today)
                            && !state
                                .progress
                                .get(&progress_map_key(&entry.id, &v.id))
                                .is_some_and(|p| p.watched)
                    })
                    .filter_map(|v| v.released.as_deref().and_then(iso_days))
                    .max()
                    .unwrap_or(0);
                new_episodes.push((newest, card));
            } else {
                list.push(card);
            }
        }
        new_episodes
            .sort_by_key(|(newest, entry)| (std::cmp::Reverse(*newest), entry.series_id.clone()));
        list.sort_by_key(|c| std::cmp::Reverse(c.updated_at_secs));
        // Preserve every stored source/history record. Only duplicate cards for
        // the same proven episode collapse, with the latest playback first.
        let continue_visible = self
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .home_row_enabled(&HomeRowSource::ContinueWatching);
        deduplicate_episode_home_rows(
            &mut list,
            &mut new_episodes,
            &episode_identities,
            continue_visible,
            CONTINUE_MAX,
        );
        let new_episodes = new_episodes.into_iter().map(|(_, entry)| entry).collect();
        let stale_map = {
            let mut state = self.shared.lock().unwrap();
            let mut latest_times = HashMap::new();
            for p in state.progress.values() {
                latest_times
                    .entry(p.series_id.as_str())
                    .and_modify(|at: &mut u64| *at = (*at).max(p.updated_at_secs))
                    .or_insert(p.updated_at_secs);
            }
            let stale: Vec<String> = state
                .continue_hidden
                .iter()
                .filter(|(id, at)| {
                    !is_home_hide_key(id)
                        && latest_times
                            .get(id.as_str())
                            .is_none_or(|updated| *updated > **at)
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in &stale {
                state.continue_hidden.remove(id);
            }
            state.new_episodes_list = new_episodes;
            state.continue_list = list;
            (!stale.is_empty()).then(|| state.continue_hidden.clone())
        };
        if let Some(map) = stale_map {
            write_continue_hidden(&map);
        }
    }

    /// Build the Home → Continue Watching rows (joined against library
    /// entries for title/poster and the episode cache for the label).
    /// Posters paint instantly when already decoded, otherwise dispatch.
    pub(super) fn current_continue_rows(&self) -> Vec<ContinueRow> {
        self.current_episode_home_rows(false)
    }

    pub(super) fn current_new_episode_rows(&self) -> Vec<ContinueRow> {
        self.current_episode_home_rows(true)
    }

    fn current_episode_home_rows(&self, new: bool) -> Vec<ContinueRow> {
        let (list, enabled) = {
            let mut state = self.shared.lock().unwrap();
            if new {
                prune_watched_new_episode_rows(&mut state);
            }
            (
                if new {
                    state.new_episodes_list.clone()
                } else {
                    state.continue_list.clone()
                },
                state.cache_settings.home_row_enabled(if new {
                    &HomeRowSource::NewEpisodes
                } else {
                    &HomeRowSource::ContinueWatching
                }),
            )
        };
        if !enabled {
            return Vec::new();
        }
        let mut drafts = Vec::with_capacity(list.len());
        for c in &list {
            let episodes = read_episodes_cache_for(&c.type_, &c.series_id).unwrap_or_default();
            let state = self.shared.lock().unwrap();
            if !state.cache_settings.home_row_enabled(if new {
                &HomeRowSource::NewEpisodes
            } else {
                &HomeRowSource::ContinueWatching
            }) {
                return Vec::new();
            }
            let episode_artwork = state.cache_settings.home_episode_artwork;
            let current = if new {
                &state.new_episodes_list
            } else {
                &state.continue_list
            };
            if !current.iter().any(|item| {
                item.series_id == c.series_id
                    && item.type_ == c.type_
                    && item.episode_id == c.episode_id
            }) {
                continue;
            }
            let Some(e) = state
                .entries
                .iter()
                .find(|e| e.id == c.series_id && e.type_ == c.type_)
            else {
                continue;
            };
            let video = episodes.iter().find(|v| v.id == c.episode_id);
            let subtitle = video
                .map(|v| format!("{} · {}", episode_badge(v), episode_row_label(v)))
                .unwrap_or_else(|| text::tr("Resume").to_string());
            let record = state
                .progress
                .get(&progress_map_key(&c.series_id, &c.episode_id));
            let fraction = record
                .map(|p| progress_fraction(p.position_secs, p.duration_secs))
                .unwrap_or(0.0);
            let started = record
                .map(|p| (!p.watched && p.position_secs > 0.0) || genuine_partial_rewatch(p))
                .unwrap_or(false);
            let is_watched = |v: &Video| {
                state
                    .progress
                    .get(&progress_map_key(&c.series_id, &v.id))
                    .is_some_and(|p| p.watched)
            };
            let badge = continue_badge(started, &c.episode_id, &episodes, is_watched);
            let art_url = home_card_art_url(&e.poster_url, video, episode_artwork);
            drafts.push(ContinueRowDraft {
                id: c.series_id.clone(),
                type_: c.type_.clone(),
                episode_id: c.episode_id.clone(),
                title: e.name.clone(),
                subtitle,
                episode_title: video.map(episode_row_label).unwrap_or_default(),
                ep_no: video.map(episode_badge).unwrap_or_default(),
                remaining: if new {
                    text::new_episodes(
                        new_episode_ids(&c.series_id, &episodes, &state.progress).len(),
                    )
                } else {
                    continue_remaining(record)
                },
                art_url,
                poster_url: e.poster_url.clone(),
                progress: if new { 0.0 } else { fraction },
                badge: if new {
                    2
                } else if badge == 2 {
                    1
                } else {
                    badge
                },
            });
        }
        drafts
            .into_iter()
            .map(|d| {
                let (poster, is_loaded) = home_card_image(&d.art_url, &d.poster_url);
                ContinueRow {
                    media_type: d.type_.into(),
                    episode_id: d.episode_id.into(),
                    id: d.id.into(),
                    title: d.title.into(),
                    subtitle: d.subtitle.into(),
                    episode_title: d.episode_title.into(),
                    ep_no: d.ep_no.into(),
                    remaining: d.remaining.into(),
                    art_url: d.art_url.into(),
                    poster,
                    is_loaded,
                    progress: d.progress,
                    badge: d.badge,
                }
            })
            .collect()
    }

    /// Push the current Continue Watching, New episodes and Upcoming rows to Home,
    /// plus the Upcoming calendar models (healed first, so a rebuild that
    /// moved air dates never leaves the calendar on a dead month/day).
    pub(super) fn apply_home_to_ui(&self) {
        self.refresh_home_watch_action();
        if let Some(app) = self.app() {
            app.set_home_continue(Rc::new(VecModel::from(self.current_continue_rows())).into());
            app.set_home_upcoming(Rc::new(VecModel::from(self.current_upcoming_rows())).into());
            app.set_home_new_episodes(
                Rc::new(VecModel::from(self.current_new_episode_rows())).into(),
            );
            let settings = self.shared.lock().unwrap().cache_settings.clone();
            if (app.get_home_view() == 1
                && !settings.home_row_enabled(&HomeRowSource::ContinueWatching))
                || (app.get_home_view() == 2
                    && !settings.home_row_enabled(&HomeRowSource::Upcoming))
                || (app.get_home_view() == -3
                    && !settings.home_row_enabled(&HomeRowSource::NewEpisodes))
            {
                app.set_home_view(0);
            }
        }
        self.apply_upcoming_cal_to_ui();
        self.refresh_home_history_models();
        self.ensure_home_catalog_rows_loaded();
        self.ensure_recommendations_loaded();
    }

    /// Republish local history immediately; addon results remain cached and
    /// their refresh cadence is independent of playback and sync application.
    pub(super) fn refresh_home_history_models(&self) {
        let groups = self.shared.lock().unwrap().home_catalog_row_groups.clone();
        self.install_home_catalog_rows(groups);
    }

    pub(super) fn refresh_prepared_home_history_models(&self) {
        let groups = self.shared.lock().unwrap().home_catalog_row_groups.clone();
        self.install_prepared_home_catalog_rows(groups);
    }

    /// Negative IDs are built-ins; nonnegative IDs address catalog sections.
    /// Cached groups own stable source identities, so missing/empty catalogs
    /// never shift another configured row into their slot.
    pub(super) fn publish_home_catalog_order(&self) {
        let Some(app) = self.app() else { return };
        let continue_count = app.get_home_continue().row_count();
        let upcoming_count = app.get_home_upcoming().row_count();
        let new_count = app.get_home_new_episodes().row_count();
        let order = {
            let state = self.shared.lock().unwrap();
            state
                .cache_settings
                .effective_home_rows()
                .into_iter()
                .filter(|row| row.enabled)
                .filter_map(|row| match row.source {
                    HomeRowSource::ContinueWatching if continue_count > 0 => Some(-1),
                    HomeRowSource::Upcoming if upcoming_count > 0 => Some(-2),
                    HomeRowSource::NewEpisodes if new_count > 0 => Some(-3),
                    HomeRowSource::WatchHistory if !state.home_history_list.is_empty() => {
                        Some(state.home_catalog_row_groups.len() as i32)
                    }
                    source @ (HomeRowSource::Addon(_) | HomeRowSource::BecauseYouWatched) => state
                        .home_catalog_row_groups
                        .iter()
                        .position(|group| group.source == source)
                        .map(|index| index as i32),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let mut positions = vec![-1; app.get_home_catalog_sections().row_count() + 3];
        for (position, id) in order.iter().enumerate() {
            let slot = match id {
                -1 => 0,
                -2 => 1,
                -3 => app.get_home_catalog_sections().row_count() + 2,
                id => *id as usize + 2,
            };
            if let Some(value) = positions.get_mut(slot) {
                *value = position as i32;
            }
        }
        app.set_home_catalog_positions(Rc::new(VecModel::from(positions)).into());
        app.set_home_catalog_order(Rc::new(VecModel::from(order)).into());
    }

    /// Refresh language-dependent Home row text without replacing poster
    /// images or disturbing the carousel models when their shape is unchanged.
    pub(super) fn refresh_home_language_text(&self) {
        self.refresh_home_watch_action();
        let Some(app) = self.app() else { return };

        for new in [false, true] {
            let rows = self.current_episode_home_rows(new);
            let model = if new {
                app.get_home_new_episodes()
            } else {
                app.get_home_continue()
            };
            if model.row_count() == rows.len() {
                for (index, fresh) in rows.into_iter().enumerate() {
                    if let Some(mut current) = model.row_data(index)
                        && (current.subtitle != fresh.subtitle
                            || current.remaining != fresh.remaining)
                    {
                        current.subtitle = fresh.subtitle;
                        current.episode_title = fresh.episode_title;
                        current.remaining = fresh.remaining;
                        model.set_row_data(index, current);
                    }
                }
            } else if new {
                app.set_home_new_episodes(Rc::new(VecModel::from(rows)).into());
            } else {
                app.set_home_continue(Rc::new(VecModel::from(rows)).into());
            }
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
        self.refresh_home_history_models();
    }

    /// Rebuild [`Shared::upcoming_list`]: unaired episodes of library
    /// series the user caught up with — every dated, released episode
    /// watched, at least one episode watched at all, and at least one
    /// episode airing in the future. Dateless episodes are invisible to this
    /// check (unknown schedule). Air-date ascending, capped so Home stays
    /// a quick glance.
    pub(super) fn rebuild_upcoming_list(&self) {
        let groups = self.library_display_groups();
        const UPCOMING_MAX: usize = 20;
        let identities: Vec<HomeLibraryIdentity> = {
            let state = self.shared.lock().unwrap();
            state
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    e.type_ != "movie"
                        && e.watch_status == WatchStatus::Auto
                        && groups.is_representative(&e.type_, &e.id)
                })
                .map(|(index, e)| HomeLibraryIdentity {
                    index,
                    type_: e.type_.clone(),
                    id: e.id.clone(),
                })
                .collect()
        };
        let today = today_days();
        let mut upcoming: Vec<UpcomingEntry> = Vec::new();
        for identity in identities {
            if !profile_policy::allows_title(
                &profiles::ProfileStore::capture(),
                &identity.type_,
                &identity.id,
            ) {
                continue;
            }
            let episodes =
                read_episodes_cache_for(&identity.type_, &identity.id).unwrap_or_default();
            let state = self.shared.lock().unwrap();
            let Some(e) = state.entries.get(identity.index).filter(|entry| {
                entry.id == identity.id
                    && entry.type_ == identity.type_
                    && entry.watch_status == WatchStatus::Auto
                    && entry.type_ != "movie"
            }) else {
                continue;
            };
            if episodes.is_empty() {
                continue;
            }
            let is_watched = |v: &Video| {
                state
                    .progress
                    .get(&progress_map_key(&e.id, &v.id))
                    .is_some_and(|p| p.watched)
            };
            if let Some((i, days)) = next_upcoming_episode(&episodes, is_watched, today) {
                upcoming.push(UpcomingEntry {
                    series_id: e.id.clone(),
                    type_: e.type_.clone(),
                    episode_id: episodes[i].id.clone(),
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
        self.current_upcoming_rows_for_day(None)
    }

    /// The Upcoming rows airing on one calendar day (epoch days). Same joins
    /// as [`Self::current_upcoming_rows`]; `index` still positions into the
    /// full Upcoming list, so card taps resolve through `upcoming_picked`.
    pub(super) fn current_upcoming_day_rows(&self, day: i64) -> Vec<UpcomingRow> {
        self.current_upcoming_rows_for_day(Some(day))
    }

    fn current_upcoming_rows_for_day(&self, day: Option<i64>) -> Vec<UpcomingRow> {
        let (list, enabled) = {
            let state = self.shared.lock().unwrap();
            (
                state.upcoming_list.clone(),
                state
                    .cache_settings
                    .home_row_enabled(&HomeRowSource::Upcoming),
            )
        };
        if !enabled {
            return Vec::new();
        }
        let mut drafts = Vec::with_capacity(list.len());
        for (i, u) in list.iter().enumerate() {
            if day.is_some_and(|day| u.air_days != day) {
                continue;
            }
            let episodes = read_episodes_cache_for(&u.type_, &u.series_id).unwrap_or_default();
            let state = self.shared.lock().unwrap();
            if !state
                .cache_settings
                .home_row_enabled(&HomeRowSource::Upcoming)
            {
                return Vec::new();
            }
            let date_relative = state.cache_settings.date_relative;
            let episode_artwork = state.cache_settings.home_episode_artwork;
            if !upcoming_entry_matches_at(&state.upcoming_list, i, u) {
                continue;
            }
            let Some(e) = state
                .entries
                .iter()
                .find(|e| e.id == u.series_id && e.type_ == u.type_)
            else {
                continue;
            };
            let Some(v) = episodes.iter().find(|v| v.id == u.episode_id) else {
                continue;
            };
            let subtitle = format!("{} · {}", episode_badge(v), episode_row_label(v));
            let date = v
                .released
                .as_deref()
                .and_then(|d| Bridge::format_human_date(d, date_relative))
                .unwrap_or_default();
            let art_url = home_card_art_url(&e.poster_url, Some(v), episode_artwork);
            drafts.push(UpcomingRowDraft {
                id: u.series_id.clone(),
                title: e.name.clone(),
                subtitle,
                date,
                episode_title: episode_row_label(v),
                ep_no: episode_badge(v),
                art_url,
                poster_url: e.poster_url.clone(),
                index: i,
            });
        }
        drafts
            .into_iter()
            .map(|d| {
                let (poster, is_loaded) = home_card_image(&d.art_url, &d.poster_url);
                UpcomingRow {
                    id: d.id.into(),
                    title: d.title.into(),
                    subtitle: d.subtitle.into(),
                    date: d.date.into(),
                    episode_title: d.episode_title.into(),
                    ep_no: d.ep_no.into(),
                    art_url: d.art_url.into(),
                    poster,
                    is_loaded,
                    index: d.index as i32,
                }
            })
            .collect()
    }

    /// Main thread: an Upcoming card was picked — open the series detail
    /// page (the episode hasn't aired, so there are no streams to jump
    /// to; the user lands on the episode list).
    pub(super) fn upcoming_picked(&self, index: usize) {
        let selected = {
            self.shared
                .lock()
                .unwrap()
                .upcoming_list
                .get(index)
                .map(|u| (u.type_.clone(), u.series_id.clone()))
        };
        let Some((type_, series_id)) = selected else {
            return;
        };
        self.episode_home_enter(&type_, &series_id);
    }

    /// Main thread: a Continue Watching card was picked — open the series
    /// detail page and jump straight to the resume episode's streams when
    /// its episode list is cached (otherwise the detail page opens
    /// normally and the user picks the episode once meta loads).
    pub(super) fn continue_picked(&self, index: usize) {
        let Some(c) = self.continue_entry(index) else {
            return;
        };
        self.episode_home_picked(c);
    }

    pub(super) fn new_episode_picked(&self, index: usize) {
        let entry = self
            .shared
            .lock()
            .unwrap()
            .new_episodes_list
            .get(index)
            .cloned();
        if let Some(entry) = entry {
            self.episode_home_picked(entry);
        }
    }

    fn episode_home_picked(&self, c: ContinueEntry) {
        if !self.episode_home_enter(&c.type_, &c.series_id) {
            return;
        }
        let is_movie = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .is_some_and(|m| m.id == c.series_id && m.type_ == c.type_ && m.type_ == "movie");
        if is_movie && let Some(app) = self.app() {
            self.watch_now();
            app.set_detail_deep_stream(true);
            return;
        }
        // Resolve the resume episode against the cached list (the rows
        // just built come from this same cache, so the index matches).
        let (season, row_idx) = {
            let state = self.shared.lock().unwrap();
            let m = match state.modal_item.as_ref() {
                Some(m) if m.id == c.series_id && m.type_ == c.type_ && !m.seasons.is_empty() => m,
                _ => return,
            };
            let videos = &m.videos;
            let v = match videos.iter().find(|v| v.id == c.episode_id) {
                Some(v) => v,
                None => return,
            };
            let season = v.season.unwrap_or(0);
            if !m.seasons.contains(&season) {
                return;
            }
            match Self::filtered_row_index(videos, season, "", &c.episode_id) {
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
                .is_some_and(|m| m.id == c.series_id && m.type_ == c.type_);
            if on_streams && still_same {
                app.set_detail_deep_stream(true);
            }
        }
    }

    pub(super) fn home_episode_action(
        &self,
        new_row: bool,
        type_: &str,
        id: &str,
        episode: &str,
        action: i32,
    ) {
        let entry = {
            let state = self.shared.lock().unwrap();
            episode_home_action_entry(&state, new_row, type_, id, episode)
        };
        let Some(entry) = entry else { return };
        match action {
            0 => self.episode_home_picked(entry),
            1 => {
                self.episode_home_enter(&entry.type_, &entry.series_id);
            }
            2 if new_row => self.remove_new_episode_entry(entry),
            2 => self.remove_continue_entry(entry),
            _ => {}
        }
    }

    pub(super) fn continue_remove(&self, index: usize) {
        if let Some(entry) = self.continue_entry(index) {
            self.remove_continue_entry(entry);
        }
    }

    fn remove_continue_entry(&self, entry: ContinueEntry) {
        let episodes = read_episodes_cache_for(&entry.type_, &entry.series_id).unwrap_or_default();
        let proofs = HomeEpisodeProofs::new(&entry.type_, &episodes);
        let map = {
            let mut state = self.shared.lock().unwrap();
            dismiss_continue(&mut state.continue_hidden, &entry, &proofs, now_secs());
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

    fn episode_home_enter(&self, type_: &str, id: &str) -> bool {
        let entry = self
            .shared
            .lock()
            .unwrap()
            .entries
            .iter()
            .find(|e| e.id == id && e.type_ == type_)
            .cloned();
        if let Some(entry) = entry {
            self.open_library_entry(entry);
            true
        } else {
            false
        }
    }

    pub(super) fn continue_enter(&self, index: usize) {
        if let Some(entry) = self.continue_entry(index) {
            self.episode_home_enter(&entry.type_, &entry.series_id);
        }
    }

    pub(super) fn new_episode_enter(&self, index: usize) {
        let entry = self
            .shared
            .lock()
            .unwrap()
            .new_episodes_list
            .get(index)
            .cloned();
        if let Some(entry) = entry {
            self.episode_home_enter(&entry.type_, &entry.series_id);
        }
    }

    pub(super) fn new_episode_remove(&self, index: usize) {
        let entry = self
            .shared
            .lock()
            .unwrap()
            .new_episodes_list
            .get(index)
            .cloned();
        if let Some(entry) = entry {
            self.remove_new_episode_entry(entry);
        }
    }

    fn remove_new_episode_entry(&self, entry: ContinueEntry) {
        let episodes = read_episodes_cache_for(&entry.type_, &entry.series_id).unwrap_or_default();
        let proofs = HomeEpisodeProofs::new(&entry.type_, &episodes);
        let ids = new_episode_ids(
            &entry.series_id,
            &episodes,
            &self.shared.lock().unwrap().progress,
        );
        let mut hidden: HashMap<String, Vec<String>> =
            read_json(NEW_EPISODES_HIDDEN_KEY).unwrap_or_default();
        dismiss_new_episode_batch(&mut hidden, &entry, ids, &proofs);
        write_json(NEW_EPISODES_HIDDEN_KEY, &hidden);
        self.rebuild_continue_list();
        self.apply_home_to_ui();
    }

    pub(super) fn show_discover_page(&self) {
        if let Some(app) = self.app() {
            app.set_show_about(false);
            app.set_show_home(false);
            app.set_show_library(false);
            app.set_show_settings(false);
        }
    }

    /// Main thread: show the Home landing page (Continue Watching).
    pub(super) fn show_home_page(&self) {
        if let Some(app) = self.app() {
            app.set_show_about(false);
            app.set_show_home(true);
            app.set_show_library(false);
            app.set_show_settings(false);
            // Tapping Home always lands on the carousels, not a remembered
            // "see all" subpage.
            app.set_home_view(0);
        }
        // Keep the same-profile models on screen while the guarded worker
        // joins history/metadata. Route feedback must not wait on cache scans.
        self.request_progress_projection();
        self.ensure_home_showcase_loaded();
        self.ensure_home_catalog_rows_loaded();
        self.refresh_saved_episode_metadata();
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
fn replace_home_showcase_list(state: &mut Shared, mut previews: Vec<MetaPreview>, index: usize) {
    for preview in &mut previews {
        restore_home_showcase_header(preview);
    }
    state.home_showcase_artwork = previews
        .iter()
        .enumerate()
        .map(|(index, _)| (index, HomeShowcaseArtwork::default()))
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
                .position(|preview| home_showcase_same_identity(preview, current))
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
            let owner = nova_providers::ExternalId::parse(&preview.id)
                .is_none()
                .then(|| {
                    preview
                        .extra
                        .get("novaSourceUrl")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                });
            if seen.insert((
                preview.type_.clone(),
                preview.id.clone(),
                owner,
                catalog_grouping::virtual_season(&preview),
            )) {
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

/// Use an episode thumbnail when enabled and available, otherwise the series poster.
/// The selected URL travels with the model so late image loads cannot replace
/// a card whose episode changed while the same series stayed in its slot.
fn home_card_art_url(poster_url: &str, video: Option<&Video>, episode_artwork: bool) -> String {
    video
        .filter(|_| episode_artwork)
        .and_then(|v| v.thumbnail.as_deref())
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .unwrap_or(poster_url.trim())
        .to_string()
}

fn home_card_image(url: &str, fallback: &str) -> (Image, bool) {
    decoded_cache_get(&image_cache_key(
        url,
        ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
    ))
    .or_else(|| {
        decoded_cache_get(&image_cache_key(
            fallback,
            ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
        ))
    })
    .map(|buf| (Image::from_rgba8(buf), true))
    .unwrap_or_else(|| (Image::default(), false))
}

// Canonical episode IDs and current metadata aliases prove individual episodes,
// never an entire franchise. Movies deliberately have no cross-source collapse.
fn episode_home_action_entry(
    state: &Shared,
    new_row: bool,
    type_: &str,
    id: &str,
    episode: &str,
) -> Option<ContinueEntry> {
    let rows = if new_row {
        &state.new_episodes_list
    } else {
        &state.continue_list
    };
    rows.iter()
        .find(|row| row.type_ == type_ && row.series_id == id && row.episode_id == episode)
        .filter(|row| {
            !new_row
                || !state
                    .progress
                    .get(&progress_map_key(&row.series_id, &row.episode_id))
                    .is_some_and(|p| p.watched)
        })
        .cloned()
}

fn home_catalog_scroll_key(source: &HomeRowSource) -> SharedString {
    serde_json::to_string(&(profiles::current_id(), source))
        .expect("Home row scroll identity")
        .into()
}

struct HomeEpisodeProofs {
    aliases: HashMap<String, Vec<String>>,
}

impl HomeEpisodeProofs {
    fn new(type_: &str, videos: &[Video]) -> Self {
        if !matches!(type_, "series" | "anime") {
            return Self {
                aliases: HashMap::new(),
            };
        }
        let compact = videos
            .iter()
            .map(|video| {
                let mut result = Video {
                    id: video.id.clone(),
                    ..Default::default()
                };
                result.extra.insert(
                    "novaStreamIds".into(),
                    serde_json::json!(confirmed_episode_stream_ids(video)),
                );
                result
            })
            .collect();
        let index = media_identity::IdentityIndex::new([media_identity::TitleInventory {
            media_type: type_.into(),
            series_id: "home-source".into(),
            videos: compact,
        }]);
        let mut aliases = HashMap::new();
        for group in index.progress_groups {
            let global = group
                .iter()
                .map(|(_, id)| id.clone())
                .filter(|id| media_identity::canonical_episode_namespace(id).is_some())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            for (_, id) in group {
                aliases.insert(id, global.clone());
            }
        }
        Self { aliases }
    }

    fn aliases(&self, type_: &str, id: &str) -> Vec<String> {
        if !matches!(type_, "series" | "anime") {
            return Vec::new();
        }
        self.aliases.get(id).cloned().unwrap_or_else(|| {
            media_identity::canonical_episode_namespace(id)
                .map(|_| vec![id.into()])
                .unwrap_or_default()
        })
    }
}

fn home_episode_identity(media_type: &str, id: &str, episodes: &[Video]) -> Vec<String> {
    HomeEpisodeProofs::new(media_type, episodes).aliases(media_type, id)
}

// Existing JSON maps/sync rows retain their value shapes. New keys distinguish
// a typed source title from a proven global episode; legacy raw IDs still read.
fn home_title_hide_key(type_: &str, id: &str) -> String {
    watch_history::title_key(type_, id)
}

fn is_home_hide_key(id: &str) -> bool {
    id.starts_with('[')
        && serde_json::from_str::<Vec<String>>(id).is_ok_and(|parts| {
            parts.len() == 2 || (parts.len() == 3 && parts[0] == "episode" && parts[1].is_empty())
        })
}

fn home_episode_hide_key(id: &str) -> String {
    serde_json::to_string(&("episode", "", id)).expect("episode dismissal identity")
}

fn continue_is_hidden(
    hidden: &HashMap<String, u64>,
    type_: &str,
    id: &str,
    aliases: &[String],
    updated: u64,
) -> bool {
    hidden
        .get(id)
        .filter(|_| !is_home_hide_key(id))
        .into_iter()
        .chain(hidden.get(&home_title_hide_key(type_, id)))
        .chain(
            aliases
                .iter()
                .filter_map(|alias| hidden.get(&home_episode_hide_key(alias))),
        )
        .any(|at| *at >= updated)
}

fn new_episodes_are_hidden(
    hidden: &HashMap<String, Vec<String>>,
    type_: &str,
    id: &str,
    available: &[String],
    proofs: &HomeEpisodeProofs,
) -> bool {
    available.iter().all(|episode| {
        hidden
            .get(id)
            .filter(|_| !is_home_hide_key(id))
            .into_iter()
            .chain(hidden.get(&home_title_hide_key(type_, id)))
            .any(|ids| ids.contains(episode))
            || proofs.aliases(type_, episode).iter().any(|alias| {
                hidden
                    .get(&home_episode_hide_key(alias))
                    .is_some_and(|ids| ids.contains(alias))
            })
    })
}

fn dismiss_continue(
    hidden: &mut HashMap<String, u64>,
    entry: &ContinueEntry,
    proofs: &HomeEpisodeProofs,
    now: u64,
) {
    let at = now.max(entry.updated_at_secs);
    for key in std::iter::once(home_title_hide_key(&entry.type_, &entry.series_id)).chain(
        proofs
            .aliases(&entry.type_, &entry.episode_id)
            .iter()
            .map(|alias| home_episode_hide_key(alias)),
    ) {
        hidden
            .entry(key)
            .and_modify(|old| *old = (*old).max(at))
            .or_insert(at);
    }
}

fn dismiss_new_episode_batch(
    hidden: &mut HashMap<String, Vec<String>>,
    entry: &ContinueEntry,
    ids: Vec<String>,
    proofs: &HomeEpisodeProofs,
) {
    for alias in ids.iter().flat_map(|id| proofs.aliases(&entry.type_, id)) {
        hidden.insert(home_episode_hide_key(&alias), vec![alias]);
    }
    hidden.insert(home_title_hide_key(&entry.type_, &entry.series_id), ids);
}

pub(super) fn clear_home_continue_hidden(state: &mut Shared) -> Option<HashMap<String, u64>> {
    let target = state.playback.as_ref()?;
    let type_ = target
        .history
        .as_ref()
        .map(|history| history.type_.as_str())
        .unwrap_or_default();
    let videos = state
        .modal_item
        .as_ref()
        .filter(|modal| modal.id == target.series_id && modal.type_ == type_)
        .map(|modal| modal.videos.as_slice())
        .unwrap_or_default();
    let aliases = home_episode_identity(type_, &target.episode_id, videos);
    let keys = std::iter::once(target.series_id.clone())
        .chain(std::iter::once(home_title_hide_key(
            type_,
            &target.series_id,
        )))
        .chain(aliases.iter().map(|alias| home_episode_hide_key(alias)))
        .collect::<Vec<_>>();
    let mut changed = false;
    for key in keys {
        changed |= state.continue_hidden.remove(&key).is_some();
    }
    changed.then(|| state.continue_hidden.clone())
}

type HomeEpisodeIdentities = HashMap<(String, String, String), Vec<String>>;

fn retain_home_episode(
    card: &ContinueEntry,
    identities: &HomeEpisodeIdentities,
    seen: &mut HashSet<String>,
) -> bool {
    let Some(aliases) = identities.get(&(
        card.type_.clone(),
        card.series_id.clone(),
        card.episode_id.clone(),
    )) else {
        return true;
    };
    let duplicate = aliases.iter().any(|alias| seen.contains(alias));
    // A suppressed source may bridge another confirmed catalog namespace.
    seen.extend(aliases.iter().cloned());
    !duplicate
}

fn deduplicate_episode_home_rows(
    continuing: &mut Vec<ContinueEntry>,
    new: &mut Vec<(i64, ContinueEntry)>,
    identities: &HomeEpisodeIdentities,
    continue_visible: bool,
    limit: usize,
) {
    let source_aliases = continuing
        .iter()
        .filter(|_| continue_visible)
        .filter_map(|card| {
            identities.get(&(
                card.type_.clone(),
                card.series_id.clone(),
                card.episode_id.clone(),
            ))
        })
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    continuing.retain(|card| retain_home_episode(card, identities, &mut seen));
    continuing.truncate(limit);
    // A disabled rail or a card outside its cap cannot stand in for a visible
    // New Episodes card. Still deduplicate each rail independently.
    seen.clear();
    if continue_visible {
        for card in continuing {
            retain_home_episode(card, identities, &mut seen);
        }
        // A suppressed duplicate may carry a second confirmed namespace for
        // a retained card. Preserve that bridge without admitting unrelated
        // episodes from cards beyond the visible cap.
        loop {
            let previous = seen.len();
            for aliases in &source_aliases {
                if aliases.iter().any(|alias| seen.contains(alias)) {
                    seen.extend(aliases.iter().cloned());
                }
            }
            if seen.len() == previous {
                break;
            }
        }
    }
    new.retain(|(_, card)| retain_home_episode(card, identities, &mut seen));
    new.truncate(limit);
}

fn continue_remaining(record: Option<&EpisodeProgress>) -> String {
    let Some(record) = record else {
        return String::new();
    };
    if record.watched && !genuine_partial_rewatch(record) {
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
pub(super) fn upcoming_row(
    entries: &[LibraryEntry],
    u: &UpcomingEntry,
    index: usize,
    date_relative: bool,
    episode_artwork: bool,
) -> Option<UpcomingRow> {
    let e = entries
        .iter()
        .find(|e| e.id == u.series_id && e.type_ == u.type_)?;
    let episodes = read_episodes_cache_for(&u.type_, &u.series_id).unwrap_or_default();
    let v = episodes.iter().find(|v| v.id == u.episode_id)?;
    let subtitle = format!("{} · {}", episode_badge(v), episode_row_label(v));
    let date = v
        .released
        .as_deref()
        .and_then(|d| Bridge::format_human_date(d, date_relative))
        .unwrap_or_default();
    let art_url = home_card_art_url(&e.poster_url, Some(v), episode_artwork);
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

fn upcoming_entry_matches_at(
    list: &[UpcomingEntry],
    index: usize,
    expected: &UpcomingEntry,
) -> bool {
    list.get(index).is_some_and(|current| {
        current.series_id == expected.series_id
            && current.type_ == expected.type_
            && current.episode_id == expected.episode_id
            && current.air_days == expected.air_days
    })
}

/// Gregorian (year, month 1–12, day) from days since the Unix epoch (Howard
/// Hinnant's `civil_from_days`). Inputs here are real air dates (≥ 1970), so
/// the era division stays non-negative.
pub(super) fn civil_from_days(z: i64) -> (i64, u32, u32) {
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
pub(super) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = ((month + 9) % 12) as i64; // Mar = 0 … Feb = 11
    let doy = (153 * mp + 2) / 5 + day as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Epoch days of the 1st of `days`' month.
pub(super) fn month_first(days: i64) -> i64 {
    let (year, month, _) = civil_from_days(days);
    days_from_civil(year, month, 1)
}

/// Weekday of an epoch day, Monday = 0 … Sunday = 6. 1970-01-01 was a
/// Thursday, hence the +3.
pub(super) fn weekday_monday0(days: i64) -> u32 {
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
pub(super) fn cal_cells(
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

// Upcoming represents a series once, using its nearest announced episode.
// Catalog episode order is not guaranteed to match the release dates.
pub(super) fn next_upcoming_episode(
    episodes: &[Video],
    watched: impl Fn(&Video) -> bool,
    today: i64,
) -> Option<(usize, i64)> {
    let (future, available, available_watched) = upcoming_tally(episodes, watched, today);
    if available == 0 || available_watched < available {
        return None;
    }
    future.into_iter().min_by_key(|(index, days)| {
        (
            *days,
            episodes[*index].season.unwrap_or(0),
            episodes[*index].episode.unwrap_or(0),
        )
    })
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
    // Completion stays sticky in stored progress. Only a captured playback
    // opening can turn that record into a resumable rewatch; a manual mark at
    // an old partial position must still advance to the next episode.
    if genuine_partial_rewatch(latest) {
        return Some(continue_native_resume_id(
            type_,
            item_id,
            &latest.episode_id,
            episodes,
        ));
    }
    if type_ == "movie" {
        return (!latest.watched && latest.position_secs > 0.0).then(|| latest.episode_id.clone());
    }
    if latest.watched {
        next_episode_to_watch(item_id, episodes?, progress).map(|v| v.id.clone())
    } else if latest.position_secs > 0.0 {
        Some(continue_native_resume_id(
            type_,
            item_id,
            &latest.episode_id,
            episodes,
        ))
    } else {
        next_episode_to_watch(item_id, episodes?, progress).map(|v| v.id.clone())
    }
}

fn continue_native_resume_id(
    type_: &str,
    item_id: &str,
    episode_id: &str,
    episodes: Option<&[Video]>,
) -> String {
    let Some(episodes) = episodes.filter(|episodes| {
        type_ != "movie" && !episodes.iter().any(|video| video.id == episode_id)
    }) else {
        return episode_id.to_owned();
    };
    // Projected aliases share playback clocks, so the latest-record tie can
    // select an alias absent from the native inventory. Resolve only through
    // the same conflict/ambiguity checks as progress reconciliation. Compact
    // videos avoid cloning descriptions/artwork merely to index identities.
    let videos = episodes
        .iter()
        .map(|video| {
            let mut compact = Video {
                id: video.id.clone(),
                ..Default::default()
            };
            compact.extra.insert(
                "novaStreamIds".into(),
                serde_json::json!(confirmed_episode_stream_ids(video)),
            );
            compact
        })
        .collect();
    let index = media_identity::IdentityIndex::new([media_identity::TitleInventory {
        media_type: type_.to_owned(),
        series_id: item_id.to_owned(),
        videos,
    }]);
    let group = index.progress_groups.iter().find(|group| {
        group
            .iter()
            .any(|(parent, id)| parent == item_id && id == episode_id)
    });
    let Some(group) = group else {
        return episode_id.to_owned();
    };
    let mut natives = episodes.iter().filter(|video| {
        group
            .iter()
            .any(|(parent, id)| parent == item_id && *id == video.id)
    });
    match (natives.next(), natives.next()) {
        (Some(video), None) => video.id.clone(),
        _ => episode_id.to_owned(),
    }
}

fn genuine_partial_rewatch(record: &EpisodeProgress) -> bool {
    record.play_count > 0
        && record
            .history
            .as_ref()
            .is_some_and(|history| history.rewatch)
        && record.position_secs.is_finite()
        && record.duration_secs.is_finite()
        && resumable_position(record.position_secs, record.duration_secs, false)
}

fn continue_playback_order(record: &EpisodeProgress) -> (u64, &str) {
    (
        record
            .history
            .as_ref()
            .map_or(record.updated_at_secs, |history| history.played_at_secs),
        &record.episode_id,
    )
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

pub(crate) const NEW_EPISODES_HIDDEN_KEY: &str = "new_episodes_hidden:v1";

fn new_episode_ids(
    id: &str,
    episodes: &[Video],
    progress: &HashMap<String, EpisodeProgress>,
) -> Vec<String> {
    let mut ids: Vec<_> = episodes
        .iter()
        .filter(|v| {
            episode_has_air_date(v)
                && episode_is_out(v, today_days())
                && !progress
                    .get(&progress_map_key(id, &v.id))
                    .is_some_and(|p| p.watched)
        })
        .map(|v| v.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dismissal_video(id: &str, alias: &str) -> Video {
        serde_json::from_value(serde_json::json!({"id":id,"novaStreamIds":[alias],
            "novaMetadataRevision":nova_providers::metadata_revision()}))
        .unwrap()
    }

    fn dismissal_card(type_: &str, parent: &str, episode: &str) -> ContinueEntry {
        ContinueEntry {
            type_: type_.into(),
            series_id: parent.into(),
            episode_id: episode.into(),
            updated_at_secs: 100,
        }
    }

    #[test]
    fn dismissal_continue_covers_proven_aliases_without_hiding_other_types_or_seasons() {
        let source = HomeEpisodeProofs::new("series", &[dismissal_video("opaque", "tt100:1:1")]);
        let alternate =
            HomeEpisodeProofs::new("series", &[dismissal_video("kitsu:10:1", "tt100:1:1")]);
        let entry = dismissal_card("series", "source", "opaque");
        let mut hidden = HashMap::new();
        dismiss_continue(&mut hidden, &entry, &source, 200);
        assert!(continue_is_hidden(
            &hidden,
            "series",
            "alternate",
            &alternate.aliases("series", "kitsu:10:1"),
            100
        ));
        assert!(!continue_is_hidden(
            &hidden,
            "series",
            "alternate",
            &alternate.aliases("series", "kitsu:10:1"),
            201
        ));
        assert!(!continue_is_hidden(
            &hidden,
            "series",
            "sequel",
            &["tt100:2:1".into()],
            100
        ));
        assert!(!continue_is_hidden(&hidden, "movie", "source", &[], 100));
        assert!(!continue_is_hidden(
            &hidden,
            "movie",
            &home_title_hide_key("series", "source"),
            &[],
            100
        ));
        hidden.insert(home_title_hide_key("movie", "source"), 300);
        let mut state = Shared {
            continue_hidden: hidden,
            playback: Some(PlaybackTarget {
                series_id: "source".into(),
                episode_id: "tt100:1:1".into(),
                history: Some(PlaybackHistory {
                    type_: "series".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(clear_home_continue_hidden(&mut state).is_some());
        assert_eq!(
            state.continue_hidden,
            HashMap::from([(home_title_hide_key("movie", "source"), 300)])
        );
        assert!(continue_is_hidden(
            &HashMap::from([("legacy".into(), 200)]),
            "series",
            "legacy",
            &[],
            100
        ));
    }

    #[test]
    fn dismissal_new_episode_batch_covers_aliases_but_allows_future_releases() {
        let source = HomeEpisodeProofs::new(
            "series",
            &[
                dismissal_video("a1", "tt100:1:1"),
                dismissal_video("a2", "tt100:1:2"),
            ],
        );
        let other = HomeEpisodeProofs::new(
            "series",
            &[
                dismissal_video("b1", "tt100:1:1"),
                dismissal_video("b2", "tt100:1:2"),
                dismissal_video("b3", "tt100:1:3"),
            ],
        );
        let entry = dismissal_card("series", "source", "a1");
        let mut hidden = HashMap::new();
        dismiss_new_episode_batch(&mut hidden, &entry, vec!["a1".into(), "a2".into()], &source);
        assert!(new_episodes_are_hidden(
            &hidden,
            "series",
            "other",
            &["b1".into(), "b2".into()],
            &other
        ));
        assert!(!new_episodes_are_hidden(
            &hidden,
            "series",
            "other",
            &["b1".into(), "b2".into(), "b3".into()],
            &other
        ));
        assert!(!new_episodes_are_hidden(
            &hidden,
            "movie",
            "source",
            &["a1".into()],
            &HomeEpisodeProofs::new("movie", &[])
        ));
        let legacy = HashMap::from([("source".into(), vec!["a1".into()])]);
        assert!(new_episodes_are_hidden(
            &legacy,
            "series",
            "source",
            &["a1".into()],
            &source
        ));
    }

    #[test]
    fn dismissal_rejects_stale_and_ambiguous_episode_evidence() {
        let entry = dismissal_card("series", "source", "a1");
        let mut stale = dismissal_video("a1", "tt100:1:1");
        stale.extra.insert(
            "novaMetadataRevision".into(),
            serde_json::json!(nova_providers::metadata_revision().wrapping_add(1)),
        );
        for videos in [
            vec![stale],
            vec![
                dismissal_video("a1", "tt100:1:1"),
                dismissal_video("a2", "tt100:1:1"),
            ],
        ] {
            let proofs = HomeEpisodeProofs::new("series", &videos);
            let mut hidden = HashMap::new();
            dismiss_continue(&mut hidden, &entry, &proofs, 200);
            assert!(!continue_is_hidden(
                &hidden,
                "series",
                "other",
                &["tt100:1:1".into()],
                100
            ));
        }
    }

    #[test]
    fn home_episode_actions_resolve_the_captured_type_and_episode_after_reordering() {
        let series = dismissal_card("series", "shared", "e1");
        let movie = dismissal_card("movie", "shared", "shared");
        let mut state = Shared {
            continue_list: vec![movie.clone(), series.clone()],
            new_episodes_list: vec![series],
            ..Default::default()
        };
        assert_eq!(
            episode_home_action_entry(&state, false, "series", "shared", "e1")
                .unwrap()
                .type_,
            "series"
        );
        state.continue_list.reverse();
        assert_eq!(
            episode_home_action_entry(&state, false, "movie", "shared", "shared")
                .unwrap()
                .episode_id,
            movie.episode_id
        );
        assert!(episode_home_action_entry(&state, false, "series", "shared", "e2").is_none());
        state.progress.insert(
            progress_map_key("shared", "e1"),
            EpisodeProgress {
                watched: true,
                ..Default::default()
            },
        );
        assert!(episode_home_action_entry(&state, true, "series", "shared", "e1").is_none());
        state.continue_list.clear();
        assert!(episode_home_action_entry(&state, false, "series", "shared", "e1").is_none());
    }

    #[test]
    fn continue_cards_collapse_only_current_confirmed_episode_aliases() {
        let native: Video = serde_json::from_value(serde_json::json!({
            "id": "native-episode",
            "novaStreamIds": ["tt100:2:1"],
            "novaMetadataRevision": nova_providers::metadata_revision()
        }))
        .unwrap();
        let card = |series: &str, episode: &str| ContinueEntry {
            series_id: series.into(),
            type_: "series".into(),
            episode_id: episode.into(),
            updated_at_secs: 1,
        };
        let native_card = card("native", "native-episode");
        let canonical = card("tt100", "tt100:2:1");
        let sequel = card("tt200", "tt200:2:1");
        let next_episode = card("tt100", "tt100:2:2");
        let cards = [&native_card, &canonical, &sequel, &next_episode];
        let identities = cards
            .iter()
            .map(|card| {
                (
                    (
                        card.type_.clone(),
                        card.series_id.clone(),
                        card.episode_id.clone(),
                    ),
                    home_episode_identity(
                        &card.type_,
                        &card.episode_id,
                        std::slice::from_ref(&native),
                    ),
                )
            })
            .collect();
        let mut seen = HashSet::new();
        assert!(retain_home_episode(&native_card, &identities, &mut seen));
        assert!(!retain_home_episode(&canonical, &identities, &mut seen));
        assert!(retain_home_episode(&sequel, &identities, &mut seen));
        assert!(retain_home_episode(&next_episode, &identities, &mut seen));
        let bridge = card("bridge", "bridge-episode");
        let kitsu = card("kitsu:42", "kitsu:42:1");
        let chain: HomeEpisodeIdentities = [
            (
                (
                    canonical.type_.clone(),
                    canonical.series_id.clone(),
                    canonical.episode_id.clone(),
                ),
                vec!["tt100:2:1".into()],
            ),
            (
                (
                    bridge.type_.clone(),
                    bridge.series_id.clone(),
                    bridge.episode_id.clone(),
                ),
                vec!["tt100:2:1".into(), "kitsu:42:1".into()],
            ),
            (
                (
                    kitsu.type_.clone(),
                    kitsu.series_id.clone(),
                    kitsu.episode_id.clone(),
                ),
                vec!["kitsu:42:1".into()],
            ),
        ]
        .into();
        let mut seen = HashSet::new();
        assert!(retain_home_episode(&canonical, &chain, &mut seen));
        assert!(!retain_home_episode(&bridge, &chain, &mut seen));
        assert!(!retain_home_episode(&kitsu, &chain, &mut seen));
        assert!(home_episode_identity("movie", "tt100", &[]).is_empty());
        let mut stale = native;
        stale.extra.insert(
            "novaMetadataRevision".into(),
            serde_json::json!(nova_providers::metadata_revision().wrapping_add(1)),
        );
        assert!(home_episode_identity("series", "native-episode", &[stale]).is_empty());
    }

    #[test]
    fn upcoming_selects_only_the_nearest_episode_of_a_caught_up_series() {
        let episodes: Vec<Video> = serde_json::from_value(serde_json::json!([
            {"id":"later", "released":"2026-10-20", "season":1, "episode":3},
            {"id":"aired", "released":"2026-10-01", "season":1, "episode":1},
            {"id":"next", "released":"2026-10-11", "season":1, "episode":2}
        ]))
        .unwrap();
        let today = days_from_civil(2026, 10, 10);
        assert_eq!(
            next_upcoming_episode(&episodes, |v| v.id == "aired", today)
                .map(|(i, _)| episodes[i].id.as_str()),
            Some("next")
        );
        assert!(next_upcoming_episode(&episodes, |_| false, today).is_none());
    }

    #[test]
    fn upcoming_projection_keeps_full_list_index_and_typed_identity() {
        let expected = UpcomingEntry {
            series_id: "series".into(),
            type_: "series".into(),
            episode_id: "episode-2".into(),
            air_days: 20_000,
        };
        let list = vec![
            UpcomingEntry {
                episode_id: "episode-1".into(),
                air_days: 19_000,
                ..expected.clone()
            },
            expected.clone(),
        ];
        assert!(upcoming_entry_matches_at(&list, 1, &expected));
        assert!(!upcoming_entry_matches_at(&list, 0, &expected));
        let mut wrong_type = expected.clone();
        wrong_type.type_ = "movie".into();
        assert!(!upcoming_entry_matches_at(&list, 1, &wrong_type));
    }

    #[test]
    fn home_sources_drop_disabled_and_removed_addons_but_keep_unavailable_installs() {
        let source = showcase_source("popular", "");
        let mut state = Shared::default();
        state.cache_settings.home_catalog_sources = vec![source.clone()];
        state.cache_settings.home_rows = Some(vec![nova_config::HomeRow {
            source: HomeRowSource::Addon(source.clone()),
            enabled: true,
        }]);
        state.installed = vec![Installed {
            url: source.addon_url.clone(),
            label: "Fixture".into(),
            enabled: true,
            configure_ok: None,
            available: false,
            generation: 1,
            manifest: serde_json::from_value(
                serde_json::json!({"id":"fixture","name":"Fixture","version":"1"}),
            )
            .unwrap(),
        }];
        assert_eq!(home_showcase_sources(&state), vec![source.clone()]);
        assert_eq!(home_catalog_row_sources(&state), vec![source]);
        state.installed[0].enabled = false;
        assert!(home_showcase_sources(&state).is_empty());
        assert!(home_catalog_row_sources(&state).is_empty());
        state.installed.clear();
        assert!(home_showcase_sources(&state).is_empty());
        assert!(home_catalog_row_sources(&state).is_empty());
    }

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
            &image_cache_key(fallback, ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE))),
            SharedPixelBuffer::<Rgba8Pixel>::new(3, 2),
        );
        let (image, loaded) = home_card_image(primary, fallback);
        assert!(loaded);
        assert_eq!(image.size().width, 3);
        decoded_cache_insert(
            &image_cache_key(primary, ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE))),
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
    fn featured_private_identity_keeps_catalog_ownership_on_refresh() {
        let mut first = showcase_preview("private-show");
        first
            .extra
            .insert("novaSourceUrl".into(), "https://first.example".into());
        let mut second = first.clone();
        second
            .extra
            .insert("novaSourceUrl".into(), "https://second.example".into());
        assert!(!home_showcase_same_identity(&first, &second));
        let state = Shared {
            home_showcase: vec![first.clone()],
            ..Default::default()
        };
        let refresh = vec![second.clone(), first.clone(), showcase_preview("next")];
        assert_eq!(home_showcase_refreshed_index(&state, &refresh, 1), 2);
        merge_home_showcase_preview(&mut first, &second);
        assert_eq!(first.extra["novaSourceUrl"], "https://first.example");
        first.id = "tt1234567".into();
        second.id.clone_from(&first.id);
        assert!(home_showcase_same_identity(&first, &second));
    }

    #[test]
    fn featured_sparse_refresh_preserves_enrichment_and_accepts_new_artwork() {
        let source = showcase_source("popular", "");
        let selected = std::slice::from_ref(&source);
        let mut cached = HomeShowcaseCache::default();
        let mut native = showcase_preview("vendor:show");
        native.poster = Some("https://images.example/native.jpg".into());
        cached.update(selected, vec![(source.clone(), Some(vec![native.clone()]))]);
        let mut detailed = native.clone();
        detailed.background = Some("https://images.example/backdrop.jpg".into());
        detailed.description = Some("Detail synopsis".into());
        detailed.release_info = Some(serde_json::json!(2025));
        detailed.runtime = Some("24 min".into());
        let requested = cached.previews(selected)[0].clone();
        cached.enrich(&requested, &detailed);
        native.poster = Some("https://images.example/new-poster.jpg".into());
        native.background = Some("  ".into());
        native.description = Some(String::new());
        cached.update(selected, vec![(source.clone(), Some(vec![native]))]);
        let previews = cached.previews(selected);
        assert_eq!(
            previews[0].poster.as_deref(),
            Some("https://images.example/new-poster.jpg")
        );
        assert_eq!(previews[0].background, detailed.background);
        assert_eq!(previews[0].description, detailed.description);
        assert_eq!(previews[0].release_info, detailed.release_info);
        assert_eq!(previews[0].runtime, detailed.runtime);
        let restarted: HomeShowcaseCache =
            serde_json::from_str(&serde_json::to_string(&cached).unwrap()).unwrap();
        assert_eq!(
            restarted.previews(selected)[0].background,
            detailed.background
        );
        assert_eq!(previews[0].extra["novaSourceUrl"], source.addon_url);
    }

    #[test]
    fn featured_artwork_reacquires_after_eviction_and_retries_failures() {
        let mut preview = showcase_preview("any-provider:show");
        preview.poster = Some("https://images.example/fallback.jpg".into());
        preview.background = Some("  ".into());
        assert_eq!(
            home_showcase_art_urls(&preview),
            ["https://images.example/fallback.jpg"]
        );
        let mut artwork = HomeShowcaseArtwork::default();
        let now = std::time::Instant::now();
        let (request, candidate, ready) = artwork.request(&preview, now);
        assert_eq!(
            request.as_deref(),
            Some("https://images.example/fallback.jpg")
        );
        assert!(candidate.is_none());
        assert!(!ready);
        assert!(
            artwork.request(&preview, now).0.is_none(),
            "in-flight downloads are not duplicated"
        );
        artwork.loading_url = None;
        // A remembered URL is only a key. If the shared LRU no longer has its
        // pixels, a later visit queues the image again.
        artwork.backdrop_url = preview.poster.clone().unwrap();
        assert_eq!(artwork.request(&preview, now).0, preview.poster);
        artwork.loading_url = None;
        artwork
            .failed_urls
            .insert(preview.poster.clone().unwrap(), now);
        let (request, candidate, ready) = artwork.request(&preview, now + Duration::from_secs(1));
        assert!(candidate.is_none());
        assert!(
            request.is_none() && ready,
            "cooldowns do not stall rotation"
        );
        assert_eq!(
            artwork.request(&preview, now + HOME_SHOWCASE_RETRY_DELAY).0,
            preview.poster
        );

        let mut unavailable = HomeShowcaseArtwork::default();
        unavailable
            .failed_urls
            .insert(preview.background.clone().unwrap(), now);
        unavailable
            .failed_urls
            .insert(preview.poster.clone().unwrap(), now);
        assert!(unavailable.request(&preview, now).0.is_none());
        assert_eq!(
            unavailable
                .request(&preview, now + HOME_SHOWCASE_RETRY_DELAY)
                .0,
            preview.poster,
            "whitespace-only background URLs are ignored in favor of the poster"
        );
        preview.poster.clone_from(&preview.background);
        assert!(home_showcase_art_urls(&preview).is_empty());
    }

    #[test]
    fn featured_candidate_survives_cache_eviction_until_slint_publish() {
        let mut preview = showcase_preview("evicted-candidate");
        preview.background = Some("https://images.example/evicted-candidate.jpg".into());
        let pixels = SharedPixelBuffer::<Rgba8Pixel>::new(7, 4);
        let mut cache_entry = Some(pixels);
        let mut artwork = HomeShowcaseArtwork::default();
        let (fetch, candidate, ready) =
            artwork.request_with(&preview, std::time::Instant::now(), |_| {
                let acquired = cache_entry.clone();
                cache_entry = None; // Simulate LRU eviction before publish.
                acquired
            });
        assert!(fetch.is_none() && ready);
        assert!(cache_entry.is_none());
        let published = home_showcase_backdrop_image(candidate, false, Image::default());
        assert_eq!((published.size().width, published.size().height), (7, 4));
    }

    #[test]
    fn same_slide_failure_keeps_current_backdrop_until_a_replacement_is_ready() {
        let current = Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::new(9, 5));
        let retained = home_showcase_backdrop_image(None, true, current.clone());
        assert_eq!((retained.size().width, retained.size().height), (9, 5));
        let next_slide = home_showcase_backdrop_image(None, false, current);
        assert_eq!((next_slide.size().width, next_slide.size().height), (0, 0));
    }

    #[test]
    fn featured_metadata_uses_catalog_ownership_and_manifest_restrictions() {
        fn installed(url: &str, prefix: &str) -> Installed {
            Installed {
                url: url.into(), label: url.into(), enabled: true, available: true,
                configure_ok: None, generation: 1,
                manifest: serde_json::from_value(serde_json::json!({
                    "id":url, "name":"Provider", "version":"1", "types":["series"],
                    "resources":["meta"], "idPrefixes": if prefix.is_empty() { vec![] } else { vec![prefix] }
                })).unwrap(),
            }
        }
        let owner = "https://owner.example";
        let mut state = Shared {
            installed: vec![
                installed("https://wildcard.example", ""),
                installed(owner, ""),
            ],
            ..Default::default()
        };
        let mut preview = showcase_preview("opaque:123");
        preview
            .extra
            .insert("novaSourceUrl".into(), serde_json::json!(owner));
        let urls = home_showcase_meta_urls(&state, &preview);
        assert_eq!(
            urls,
            [Addon::new(owner).unwrap().meta_url("series", &preview.id)]
        );
        preview.id = "tt123".into();
        assert_eq!(home_showcase_meta_urls(&state, &preview).len(), 2);
        assert!(home_showcase_meta_urls(&state, &preview)[0].starts_with(owner));
        state.installed[1].available = false;
        assert_eq!(home_showcase_meta_urls(&state, &preview).len(), 1);
        state.installed[0].enabled = false;
        assert!(home_showcase_meta_urls(&state, &preview).is_empty());
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
        let page = (0..HOME_RAW_CANDIDATES_PER_CATALOG + 20)
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
                .all(|catalog| catalog.previews.len() == HOME_RAW_CANDIDATES_PER_CATALOG)
        );
        let previews = cached.previews(&selected);
        assert_eq!(previews.len(), 10);
        assert_eq!(previews[5].id, "title-5");
        assert_eq!(previews[9].id, "title-9");
    }

    #[test]
    fn featured_list_reacquires_shared_artwork_by_url_and_anchors_navigation_by_identity() {
        let mut old = showcase_preview("old");
        old.background = Some("https://images.example/old.jpg".into());
        let mut state = Shared {
            home_showcase: vec![old.clone(), showcase_preview("other")],
            home_showcase_displayed: Some(old.clone()),
            home_showcase_list_generation: 7,
            home_showcase_artwork: HashMap::from([(
                0,
                HomeShowcaseArtwork {
                    backdrop_url: "https://images.example/old.jpg".into(),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let fresh = vec![showcase_preview("new"), old, showcase_preview("next")];
        assert_eq!(home_showcase_refreshed_index(&state, &fresh, 1), 2);
        assert_eq!(home_showcase_refreshed_index(&state, &fresh, -1), 0);
        replace_home_showcase_list(&mut state, fresh, 2);
        assert_eq!(state.home_showcase_list_generation, 8);
        assert_eq!(state.home_showcase_artwork[&1].backdrop_url, "");
        let reentered = state.home_showcase[1].clone();
        let (request, _pixels, ready) = state
            .home_showcase_artwork
            .get_mut(&1)
            .unwrap()
            .request(&reentered, std::time::Instant::now());
        assert_eq!(request.as_deref(), Some("https://images.example/old.jpg"));
        assert!(!ready);
        assert_eq!(
            state.home_showcase_displayed.as_ref().unwrap().id,
            "old",
            "actions retain the visible preview until new artwork commits"
        );
        assert!(state.home_showcase_artwork.contains_key(&0));
        assert_eq!(state.home_showcase_artwork[&0].backdrop_url, "");
        assert_eq!(home_showcase_refreshed_index(&state, &[], 1), 0);
    }

    #[test]
    fn featured_logo_pending_and_current_pixels_survive_cache_eviction() {
        const TEST_DIR: &str = "NOVA_HOME_LOGO_OWNERSHIP_TEST_DIR";
        let Some(root) = std::env::var_os(TEST_DIR) else {
            let root = std::env::temp_dir().join(format!(
                "nova-home-logo-test-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(TEST_DIR, &root)
                .args(["--exact", "app::home::tests::featured_logo_pending_and_current_pixels_survive_cache_eviction", "--nocapture"])
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
        let mut preview = showcase_preview("logo-title");
        preview.logo = Some("https://images.example/logo-owned-by-visible-slide.png".into());
        let generation = 7;
        {
            let mut state = bridge.shared.lock().unwrap();
            state.home_showcase = vec![preview.clone()];
            state.home_showcase_list_generation = generation;
            state.home_showcase_pending_index = Some(0);
            state
                .home_showcase_artwork
                .insert(0, HomeShowcaseArtwork::default());
        }
        let mut logo = SharedPixelBuffer::<Rgba8Pixel>::new(3, 2);
        logo.make_mut_bytes().fill(255);
        logo.make_mut_bytes()[3] = 0;
        decoded_cache_insert(
            &image_cache_key(preview.logo.as_ref().unwrap(), ImageUse::Fixed(None)),
            logo,
        );
        bridge.ensure_home_showcase_logo(0, generation);
        assert_eq!(app.get_home_pending_logo().size().width, 3);
        assert_eq!(
            app.get_home_featured_logo().size().width,
            0,
            "pending title must not paint onto the previous slide"
        );
        let identity = app.get_home_pending_logo_identity();
        decoded_cache_clear();
        bridge.commit_home_showcase_index(0, generation, Some(SharedPixelBuffer::new(1, 1)));
        assert_eq!(app.get_home_featured_title(), "logo-title");
        assert_eq!(app.get_home_featured_logo().size().width, 3);
        assert_eq!(app.get_home_featured_logo_identity(), identity);
        assert_eq!(app.get_home_pending_logo().size().width, 0);
        assert!(app.get_home_pending_logo_identity().is_empty());
        bridge.ensure_home_showcase_logo(0, generation);
        assert!(
            !bridge.shared.lock().unwrap().home_showcase_artwork[&0].logo_loading,
            "matching UI-owned logo must prevent a duplicate fetch"
        );
        assert_eq!(app.get_home_featured_logo().size().width, 3);
        // A same-slide metadata/backdrop commit must retain the live logo too.
        bridge.shared.lock().unwrap().home_showcase_pending_index = Some(0);
        bridge.commit_home_showcase_index(0, generation, Some(SharedPixelBuffer::new(2, 2)));
        assert_eq!(app.get_home_featured_logo().size().width, 3);
        app.set_home_pending_logo(app.get_home_featured_logo());
        app.set_home_pending_logo_identity(identity.clone());
        bridge.shared.lock().unwrap().home_showcase_pending_index = Some(1);
        bridge.invalidate_home_pending_logo();
        assert_eq!(
            app.get_home_pending_logo().size().width,
            0,
            "target changes release parked pixels"
        );
        bridge.shared.lock().unwrap().home_showcase_pending_index = Some(0);
        app.set_home_pending_logo(app.get_home_featured_logo());
        app.set_home_pending_logo_identity(identity);
        let mut settings = active_cache_settings();
        settings.enabled = true;
        settings.quality = if settings.quality == 80 { 81 } else { 80 };
        set_cache_settings(settings);
        bridge.invalidate_home_pending_logo();
        assert_eq!(
            app.get_home_pending_logo().size().width,
            0,
            "recipe/revision changes release parked pixels"
        );
        assert!(app.get_home_pending_logo_identity().is_empty());

        // A fallback selected because the preferred background failed must
        // stop after direct delivery, even when no decoded image can reside
        // in the LRU. Its unchanged preference is not a metadata upgrade.
        let fallback = "https://images.example/failed-background-fallback.jpg";
        let preferred = "https://images.example/failed-preferred-background.jpg";
        decoded_cache_clear();
        set_decoded_cache_budget(0);
        {
            let mut state = bridge.shared.lock().unwrap();
            let mut preview = showcase_preview("fallback-title");
            preview.background = Some(preferred.into());
            preview.poster = Some(fallback.into());
            state.home_showcase = vec![preview];
            state.home_showcase_pending_index = Some(0);
            let artwork = state.home_showcase_artwork.get_mut(&0).unwrap();
            artwork.loading_url = Some(fallback.into());
            artwork
                .failed_urls
                .insert(preferred.into(), std::time::Instant::now());
        }
        bridge.finish_home_showcase_artwork_request(
            HomeBackdropRequest {
                index: 0,
                generation,
                image_revision: image_settings_revision(),
                url: fallback.into(),
                preferred_url: preferred.into(),
            },
            Some(SharedPixelBuffer::new(4, 2)),
        );
        assert_eq!(app.get_home_featured_backdrop().size().width, 4);
        assert!(
            bridge.shared.lock().unwrap().home_showcase_artwork[&0]
                .loading_url
                .is_none()
        );
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .home_showcase_pending_index
                .is_none()
        );
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
        write_json(HOME_CATALOG_ROWS_CACHE_KEY, &cached);

        // Startup restores cached addon manifests before Home preferences.
        // That bulk restore must not prune catalogs using temporary defaults.
        let saved_settings = {
            let mut state = bridge.shared.lock().unwrap();
            state.loading_addons = true;
            std::mem::take(&mut state.cache_settings)
        };
        write_cached_manifest_for(
            &source.addon_url,
            &serde_json::from_value(
                serde_json::json!({"id":"home-fixture", "name":"Fixture", "version":"1"}),
            )
            .unwrap(),
        );
        bridge.install_persisted(&source.addon_url, true, Some(false), None);
        assert_eq!(
            read_json::<HomeShowcaseCache>(HOME_SHOWCASE_CACHE_KEY)
                .unwrap()
                .catalogs
                .len(),
            1,
            "bulk addon restore must preserve cached Home catalogs"
        );
        assert_eq!(
            read_json::<HomeShowcaseCache>(HOME_CATALOG_ROWS_CACHE_KEY)
                .unwrap()
                .catalogs
                .len(),
            1,
            "bulk addon restore must also preserve cached poster rails"
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            state.loading_addons = false;
            state.cache_settings = saved_settings;
            state.installed[0].available = false;
        }
        bridge.refresh_home_showcase();
        // Cache decoding now happens off-thread. Drain queued UI deliveries
        // without advancing the virtual paging/animation clock used below.
        fn await_cache(app: slint::Weak<AppWindow>, deadline: std::time::Instant) {
            assert!(
                std::time::Instant::now() < deadline,
                "cached Home showcase did not arrive"
            );
            if app.upgrade().unwrap().get_home_featured_count() == 2 {
                slint::quit_event_loop().unwrap();
            } else {
                slint::invoke_from_event_loop(move || await_cache(app, deadline)).unwrap();
            }
        }
        let weak = app.as_weak();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        slint::invoke_from_event_loop(move || await_cache(weak, deadline)).unwrap();
        slint::run_event_loop().unwrap();
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
        bridge.finish_home_showcase_artwork(
            0,
            old_art_generation,
            image_settings_revision(),
            String::new(),
            None,
        );
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
        let weak = app.as_weak();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        slint::invoke_from_event_loop(move || await_cache(weak, deadline)).unwrap();
        slint::run_event_loop().unwrap();
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

        // Any poster-only catalog paints immediately without enrichment. A
        // successful detail later upgrades the visible slide, survives sparse
        // catalog refreshes, and rejects stale artwork/metadata completions.
        fn featured_image_size(app: &AppWindow) -> (u32, u32) {
            let size = app.get_home_featured_backdrop().size();
            (size.width, size.height)
        }
        let mut native = showcase_preview("poster-only:one");
        native
            .extra
            .insert("novaSourceUrl".into(), serde_json::json!(source.addon_url));
        native.poster = Some("https://images.example/home-native-fixture.jpg".into());
        decoded_cache_insert(
            &image_cache_key(native.poster.as_ref().unwrap(), ImageUse::Fixed(None)),
            SharedPixelBuffer::new(2, 2),
        );
        let mut cached = HomeShowcaseCache::default();
        cached.update(
            std::slice::from_ref(&source),
            vec![(source.clone(), Some(vec![native.clone()]))],
        );
        write_json(HOME_SHOWCASE_CACHE_KEY, &cached);
        bridge.install_home_showcase(cached.previews(std::slice::from_ref(&source)));
        assert_eq!(featured_image_size(&app), (2, 2));
        let current_generation = bridge.shared.lock().unwrap().home_showcase_list_generation;
        let mut detail = MetaItem {
            preview: native.clone(),
            ..Default::default()
        };
        detail.preview.background = Some("https://images.example/home-detail-fixture.jpg".into());
        detail.preview.description = Some("Fetched through the shared detail pipeline".into());
        decoded_cache_insert(
            &image_cache_key(
                detail.preview.background.as_ref().unwrap(),
                ImageUse::Fixed(None),
            ),
            SharedPixelBuffer::new(3, 2),
        );
        bridge.finish_home_showcase_metadata(
            0,
            current_generation,
            nova_providers::metadata_revision(),
            native.clone(),
            VecDeque::new(),
            Some(detail.clone()),
        );
        assert_eq!(featured_image_size(&app), (3, 2));
        assert_eq!(
            app.get_home_featured_description(),
            detail.preview.description.clone().unwrap()
        );
        bridge.finish_home_showcase_artwork(
            0,
            current_generation,
            image_settings_revision(),
            native.poster.clone().unwrap(),
            None,
        );
        assert_eq!(
            featured_image_size(&app),
            (3, 2),
            "obsolete URL completions cannot clear an upgraded image"
        );
        let cached = read_json::<HomeShowcaseCache>(HOME_SHOWCASE_CACHE_KEY).unwrap();
        assert_eq!(
            cached.previews(std::slice::from_ref(&source))[0].background,
            detail.preview.background
        );
        let header = read_meta_header_for("series", &native.id).unwrap();
        assert_eq!(
            header.background_url,
            detail.preview.background.clone().unwrap()
        );
        let mut replacement = MetaItem {
            preview: native.clone(),
            ..Default::default()
        };
        replacement.preview.background = Some("https://images.example/new-header.jpg".into());
        merge_meta_header_for("series", &native.id, &meta_header_from_item(&replacement));
        assert_eq!(
            read_meta_header_for("series", &native.id)
                .unwrap()
                .background_url,
            replacement.preview.background.unwrap()
        );
        bridge.install_home_showcase(vec![showcase_preview("replacement")]);
        bridge.finish_home_showcase_metadata(
            0,
            current_generation,
            nova_providers::metadata_revision(),
            native,
            VecDeque::new(),
            Some(detail),
        );
        assert_eq!(app.get_home_featured_title(), "replacement");
        assert_eq!(featured_image_size(&app), (0, 0));
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
            history: None,
            manual_watch_action: None,
        }
    }

    fn library_entry(id: &str, type_: &str, name: &str, watch_status: WatchStatus) -> LibraryEntry {
        LibraryEntry {
            id: id.into(),
            type_: type_.into(),
            name: name.into(),
            year: String::new(),
            poster_url: String::new(),
            background_url: String::new(),
            genres: Vec::new(),
            description: String::new(),
            categories: Vec::new(),
            watch_status,
            imported_tracking_status: None,
            added_at_secs: 1,
        }
    }

    fn fixture_release(offset: i64) -> String {
        let (year, month, day) = civil_from_days(today_days() + offset);
        format!("{year:04}-{month:02}-{day:02}")
    }

    #[test]
    fn completed_sync_rows_cannot_remain_in_new_episode_actions() {
        let mut state = Shared::default();
        state.entries = vec![
            library_entry("zom", "series", "Zom 100", WatchStatus::Auto),
            library_entry("returning", "series", "Returning", WatchStatus::Auto),
        ];
        for n in 1..=14 {
            let episode = format!("zom:{n}");
            state.progress.insert(
                progress_map_key("zom", &episode),
                progress("zom", &episode, 0.0, true, 50),
            );
        }
        state.new_episodes_list = vec![
            ContinueEntry {
                series_id: "zom".into(),
                type_: "series".into(),
                episode_id: "zom:1".into(),
                updated_at_secs: 1,
            },
            ContinueEntry {
                series_id: "returning".into(),
                type_: "series".into(),
                episode_id: "returning:2".into(),
                updated_at_secs: 2,
            },
        ];
        prune_watched_new_episode_rows(&mut state);
        assert_eq!(state.new_episodes_list.len(), 1);
        assert_eq!(
            state.new_episodes_list[0].series_id, "returning",
            "removing a stale row must keep subsequent action indexes aligned"
        );
        assert!(
            state.progress.values().all(|record| record.watched),
            "projection must never change watched history"
        );
    }

    #[test]
    fn home_projection_rebuilds_use_current_typed_rows_and_preserve_day_indices() {
        const TEST_DIR: &str = "NOVA_HOME_PROJECTION_TEST_DIR";
        let Some(root) = std::env::var_os(TEST_DIR) else {
            let root = std::env::temp_dir().join(format!(
                "nova-home-projection-test-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(TEST_DIR, &root)
                .args(["--exact", "app::home::tests::home_projection_rebuilds_use_current_typed_rows_and_preserve_day_indices", "--nocapture"])
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

        let aired = fixture_release(-1);
        let future_a = fixture_release(2);
        let future_b = fixture_release(3);
        let future_dupe = fixture_release(4);
        let episodes = |id: &str, future: &str| {
            vec![
                Video {
                    id: format!("{id}:old"),
                    name: "Old".into(),
                    season: Some(1),
                    episode: Some(1),
                    released: Some(aired.clone()),
                    ..Default::default()
                },
                Video {
                    id: format!("{id}:future"),
                    name: "Future".into(),
                    season: Some(1),
                    episode: Some(2),
                    released: Some(future.into()),
                    ..Default::default()
                },
            ]
        };
        write_episodes_cache_for("series", "up-a", &episodes("up-a", &future_a));
        write_episodes_cache_for("series", "up-b", &episodes("up-b", &future_b));
        write_episodes_cache_for("series", "up-c", &episodes("up-c", &future_b));
        write_episodes_cache_for("series", "typed", &episodes("typed", &future_dupe));
        write_episodes_cache_for("series", "cont", &[video("cont:1", 1, 1, Some(&aired))]);
        write_episodes_cache_for("series", "stale", &[video("stale:1", 1, 1, Some(&aired))]);
        write_episodes_cache_for("series", "kept", &[video("kept:1", 1, 1, Some(&aired))]);
        write_episodes_cache_for("series", "manual", &[video("manual:1", 1, 1, Some(&aired))]);
        write_episodes_cache_for(
            "series",
            "returning",
            &[
                video("returning:old", 1, 1, Some(&fixture_release(-10))),
                video("returning:new", 1, 2, Some(&aired)),
            ],
        );

        let mut history = HashMap::new();
        for (series, episode, at, watched, position) in [
            ("cont", "cont:1", 40, false, 600.0),
            ("movie", "movie", 50, false, 300.0),
            ("stale", "stale:1", 11, false, 600.0),
            ("kept", "kept:1", 19, false, 600.0),
            ("manual", "manual:1", 60, false, 600.0),
            ("returning", "returning:old", 25, true, 3600.0),
            ("up-a", "up-a:old", 30, true, 3600.0),
            ("up-b", "up-b:old", 31, false, 600.0),
            ("up-c", "up-c:old", 32, true, 3600.0),
            ("typed", "typed:old", 33, true, 3600.0),
        ] {
            let record = progress(series, episode, position, watched, at);
            history.insert(progress_map_key(series, episode), record);
        }
        write_json(EPISODE_PROGRESS_KEY, &history);
        {
            let mut state = bridge.shared.lock().unwrap();
            state.entries = vec![
                library_entry("cont", "series", "Continue", WatchStatus::Auto),
                library_entry("movie", "movie", "Movie", WatchStatus::Auto),
                library_entry("stale", "series", "Stale hide", WatchStatus::Auto),
                library_entry("kept", "series", "Kept hide", WatchStatus::Auto),
                library_entry("manual", "series", "Manual", WatchStatus::OnHold),
                library_entry("returning", "series", "Returning", WatchStatus::Auto),
                library_entry("up-a", "series", "Up A", WatchStatus::Auto),
                library_entry("up-b", "series", "Not caught up", WatchStatus::Auto),
                library_entry("up-c", "series", "Up C", WatchStatus::Auto),
                library_entry("typed", "movie", "Wrong typed entry", WatchStatus::Auto),
                library_entry("typed", "series", "Typed TV", WatchStatus::Auto),
            ];
            state.progress = history;
            state.continue_hidden = HashMap::from([("stale".into(), 10), ("kept".into(), 20)]);
        }
        write_json(
            VIEWING_ACTIVITY_KEY,
            &HashMap::from([
                (
                    "cont".to_string(),
                    ViewingActivity {
                        phase: ViewingPhase::Active,
                    },
                ),
                (
                    "returning".to_string(),
                    ViewingActivity {
                        phase: ViewingPhase::AwaitingReturn,
                    },
                ),
            ]),
        );
        bridge.rebuild_continue_list();
        {
            let state = bridge.shared.lock().unwrap();
            assert_eq!(
                state
                    .continue_list
                    .iter()
                    .map(|e| (e.series_id.as_str(), e.type_.as_str()))
                    .collect::<Vec<_>>(),
                vec![
                    ("movie", "movie"),
                    ("cont", "series"),
                    ("up-b", "series"),
                    ("stale", "series")
                ]
            );
            assert!(
                !state
                    .continue_list
                    .iter()
                    .any(|e| e.series_id == "manual" || e.series_id == "kept")
            );
            assert!(!state.continue_hidden.contains_key("stale"));
            assert_eq!(state.continue_hidden.get("kept"), Some(&20));
        }
        let continue_rows = bridge.current_continue_rows();
        let continue_row = continue_rows.iter().find(|row| row.id == "cont").unwrap();
        assert_eq!(continue_row.progress, 1.0 / 6.0_f32);
        assert!(continue_rows.iter().any(|row| row.id == "movie"));
        assert!(
            !continue_rows
                .iter()
                .any(|row| row.id == "manual" || row.id == "kept")
        );
        let returning_rows = bridge.current_new_episode_rows();
        assert_eq!(returning_rows.len(), 1);
        assert_eq!(returning_rows[0].id, "returning");
        assert_eq!(returning_rows[0].badge, 2);
        assert_eq!(returning_rows[0].remaining, "1 new episode");

        bridge.rebuild_upcoming_list();
        let upcoming = bridge.current_upcoming_rows();
        assert_eq!(
            upcoming
                .iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Up A", "Up C", "Typed TV"]
        );
        assert_eq!(
            upcoming.iter().map(|row| row.index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        let selected_day = bridge.current_upcoming_day_rows(today_days() + 3);
        assert_eq!(selected_day.len(), 1);
        assert_eq!(selected_day[0].title, "Up C");
        assert_eq!(
            selected_day[0].index, 1,
            "calendar filtering preserves the full Upcoming list index"
        );
        assert!(
            !upcoming
                .iter()
                .any(|row| row.title == "Wrong typed entry" || row.title == "Not caught up")
        );

        let activity = read_viewing_activity();
        assert_eq!(
            activity.get("cont").map(|v| v.phase),
            Some(ViewingPhase::Active)
        );
        assert_eq!(
            activity.get("up-a").map(|v| v.phase),
            Some(ViewingPhase::AwaitingReturn)
        );
        // A delayed Home result must not resurrect a fully watched episode
        // after peer progress changes, even without navigating to Library.
        let metadata = super::super::progress_projection::Metadata::new(
            |type_, id| {
                (
                    read_episodes_cache_for(type_, id),
                    read_meta_header_for(type_, id),
                )
            },
            || true,
        );
        let groups = bridge.library_display_groups();
        let pending = {
            let mut state = bridge.shared.lock().unwrap();
            prepare_progress_home(
                &mut state,
                &metadata,
                &read_viewing_activity(),
                &HashMap::new(),
                &groups,
            )
        };
        assert_eq!(pending.new.len(), 1);
        {
            let mut state = bridge.shared.lock().unwrap();
            let episode = "returning:new";
            state.progress.insert(
                progress_map_key("returning", episode),
                progress("returning", episode, 0.0, true, 100),
            );
        }
        // Publish the already-prepared stale row against the latest progress.
        bridge.publish_progress_home(pending);
        assert_eq!(app.get_home_new_episodes().row_count(), 0);
        assert!(bridge.shared.lock().unwrap().new_episodes_list.is_empty());
        assert!(bridge.current_new_episode_rows().is_empty());
        verify_history_catalog_projection(&bridge, &app);
        // Raw IDs may be shared by different media types. Enter must use the
        // painted type even when the other Library entry precedes it.
        {
            let mut state = bridge.shared.lock().unwrap();
            state.entries = vec![
                library_entry("shared-detail", "movie", "Film", WatchStatus::Auto),
                library_entry("shared-detail", "series", "Series", WatchStatus::Auto),
            ];
            state.continue_list = vec![dismissal_card("series", "shared-detail", "e1")];
            state.new_episodes_list =
                vec![dismissal_card("movie", "shared-detail", "shared-detail")];
        }
        bridge.continue_enter(0);
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .type_,
            "series"
        );
        bridge.new_episode_enter(0);
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .type_,
            "movie"
        );
        let token = bridge
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .unwrap()
            .open_token
            .clone();
        assert!(!bridge.episode_home_enter("anime", "shared-detail"));
        assert!(Arc::ptr_eq(
            &token,
            &bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .open_token
        ));
    }

    /// Runs in the projection test's isolated UI/storage process above.
    fn verify_history_catalog_projection(bridge: &Bridge, app: &AppWindow) {
        let a = showcase_source("history-a", "");
        let b = showcase_source("history-b", "");
        let group = |source: &HomeCatalogSource, id: &str| HomeCatalogGroup {
            source: HomeRowSource::Addon(source.clone()),
            title: id.into(),
            previews: vec![showcase_preview(id)],
        };
        let mut outside = progress("outside-library", "episode-2", 600.0, false, 20);
        outside.history = Some(PlaybackHistory {
            type_: "series".into(),
            title: "Outside Library".into(),
            episode_label: "Second episode".into(),
            season: Some(1),
            episode: Some(2),
            played_at_secs: 20,
            ..Default::default()
        });
        let mut continued = progress("history-movie", "history-movie", 60.0, false, 10);
        continued.history = Some(PlaybackHistory {
            type_: "movie".into(),
            title: "Continued movie".into(),
            played_at_secs: 10,
            ..Default::default()
        });
        {
            let mut state = bridge.shared.lock().unwrap();
            state.entries = vec![library_entry(
                "history-movie",
                "movie",
                "Continued movie",
                WatchStatus::Auto,
            )];
            state.progress = [outside, continued]
                .into_iter()
                .map(|record| {
                    (
                        progress_map_key(&record.series_id, &record.episode_id),
                        record,
                    )
                })
                .collect();
            state.continue_list = vec![ContinueEntry {
                series_id: "history-movie".into(),
                type_: "movie".into(),
                episode_id: "history-movie".into(),
                updated_at_secs: 10,
            }];
            state.cache_settings.home_rows = Some(vec![
                nova_config::HomeRow {
                    source: HomeRowSource::WatchHistory,
                    enabled: true,
                },
                nova_config::HomeRow {
                    source: HomeRowSource::Addon(b.clone()),
                    enabled: true,
                },
                nova_config::HomeRow {
                    source: HomeRowSource::Addon(a.clone()),
                    enabled: true,
                },
                nova_config::HomeRow {
                    source: HomeRowSource::ContinueWatching,
                    enabled: true,
                },
            ]);
        }
        app.set_home_view(0);
        app.set_home_continue(Rc::new(VecModel::from(bridge.current_continue_rows())).into());
        bridge.install_home_catalog_rows(vec![group(&a, "addon-a"), group(&b, "addon-b")]);
        assert_eq!(
            app.get_home_catalog_order().iter().collect::<Vec<_>>(),
            vec![2, 1, 0, -1]
        );
        assert_eq!(app.get_home_catalog_sections().row_count(), 3);
        assert_eq!(app.get_home_catalog_cards().row_count(), 3);
        let history_card = app.get_home_catalog_cards().row_data(2).unwrap();
        assert_eq!(history_card.section_index, 2);
        assert_eq!(history_card.flat_index, 2);
        assert_eq!(history_card.id, "outside-library");
        assert_eq!(history_card.year, "S1 E2 · Second episode");
        assert_eq!(history_card.history_action, "Resume");
        assert_eq!(app.get_home_catalog_positions().row_data(4), Some(0));
        {
            let state = bridge.shared.lock().unwrap();
            assert_eq!(state.home_catalog_row_items.len(), 2);
            assert_eq!(state.home_history_list.len(), 1);
            assert_eq!(
                state.home_history_list
                    [(history_card.flat_index as usize) - state.home_catalog_row_items.len()]
                .episode_id,
                "episode-2"
            );
        }

        // The synthetic section follows the addon groups physically, while
        // the configured landing-row order remains independent of that index.
        app.set_home_view(5);
        bridge.install_home_catalog_rows(vec![group(&b, "addon-b")]);
        assert_eq!(app.get_home_view(), 4);
        bridge.install_home_catalog_rows(vec![group(&b, "addon-b"), group(&a, "addon-a")]);
        assert_eq!(app.get_home_view(), 5);
        app.set_home_view(3);
        bridge.install_home_catalog_rows(vec![group(&a, "addon-a"), group(&b, "addon-b")]);
        assert_eq!(app.get_home_view(), 4, "addon grid retains its source too");

        app.set_home_view(5);
        bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .home_rows
            .as_mut()
            .unwrap()[3]
            .enabled = false;
        bridge.refresh_home_history_models();
        assert_eq!(
            bridge.shared.lock().unwrap().home_history_list.len(),
            2,
            "disabled Continue cannot suppress history even with its stale model still present"
        );
        let resumed = app.get_home_catalog_cards().row_data(3).unwrap();
        assert_eq!(resumed.history_action, "Resume");
        assert!(resumed.progress > 0.0);
        assert_eq!(resumed.flat_index, 3);

        bridge
            .shared
            .lock()
            .unwrap()
            .cache_settings
            .home_rows
            .as_mut()
            .unwrap()[0]
            .enabled = false;
        bridge.refresh_home_history_models();
        assert_eq!(app.get_home_view(), 0);
        assert_eq!(app.get_home_catalog_sections().row_count(), 2);
        assert_eq!(app.get_home_catalog_cards().row_count(), 2);
        assert!(bridge.shared.lock().unwrap().home_history_list.is_empty());
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
    fn continue_alias_clock_tie_resumes_the_confirmed_native_episode() {
        let mut native = video("kitsu:10:1", 2, 1, Some("2020-01-01"));
        native
            .extra
            .insert("novaStreamIds".into(), serde_json::json!(["tt100:2:1"]));
        let episodes = vec![native, video("kitsu:10:2", 2, 2, Some("2020-01-02"))];
        let source = progress("show", "kitsu:10:1", 600.0, false, 10);
        let mut alias = source.clone();
        alias.episode_id = "tt100:2:1".into();
        assert!(continue_playback_order(&alias) > continue_playback_order(&source));
        let records = HashMap::from([
            (progress_map_key("show", &source.episode_id), source),
            (progress_map_key("show", &alias.episode_id), alias.clone()),
        ]);
        for rewatch in [false, true] {
            let mut latest = alias.clone();
            latest.watched = rewatch;
            latest.history = Some(PlaybackHistory {
                rewatch,
                ..Default::default()
            });
            assert_eq!(
                continue_resume_id("series", "show", &latest, Some(&episodes), &records),
                Some("kitsu:10:1".into()),
                "both first playback and a real rewatch retain native routing"
            );
        }
        assert_eq!(records[&progress_map_key("show", "tt100:2:1")], alias);
    }

    #[test]
    fn continue_native_resolution_rejects_stale_and_ambiguous_aliases() {
        let mut native = video("kitsu:10:1", 1, 1, None);
        native
            .extra
            .insert("novaStreamIds".into(), serde_json::json!(["tt100:1:1"]));
        let mut stale = native.clone();
        stale.extra.insert(
            "novaMetadataRevision".into(),
            serde_json::json!(nova_providers::metadata_revision().wrapping_add(1)),
        );
        let mut other = native.clone();
        other.id = "kitsu:10:2".into();
        let mut conflicting = native.clone();
        conflicting.extra.insert(
            "novaStreamIds".into(),
            serde_json::json!(["tt100:1:1", "tt200:1:1"]),
        );
        for episodes in [vec![stale], vec![native.clone(), other], vec![conflicting]] {
            assert_eq!(
                continue_native_resume_id("series", "show", "tt100:1:1", Some(&episodes)),
                "tt100:1:1",
                "unproven aliases retain the original playback identity"
            );
        }
        let episodes = [native];
        for (type_, id) in [
            ("movie", "tt100:1:1"),
            ("series", "tt100:2:1"),
            ("series", "kitsu:10:1"),
        ] {
            assert_eq!(
                continue_native_resume_id(type_, "show", id, Some(&episodes)),
                id
            );
        }
        assert_eq!(
            continue_native_resume_id("series", "show", "tt100:1:1", None),
            "tt100:1:1"
        );
    }

    #[test]
    fn new_episode_exclusion_uses_only_visible_capped_continue_cards() {
        let card = |parent: &str, episode: &str| ContinueEntry {
            series_id: parent.into(),
            type_: "series".into(),
            episode_id: episode.into(),
            updated_at_secs: 1,
        };
        let first = card("first", "tt100:1:1");
        let overflow = card("overflow", "tt200:1:1");
        let first_alias = card("first-alias", "tt100:1:1");
        let overflow_alias = card("overflow-alias", "tt200:1:1");
        let mut identities: HomeEpisodeIdentities =
            [&first, &overflow, &first_alias, &overflow_alias]
                .into_iter()
                .map(|card| {
                    (
                        (
                            card.type_.clone(),
                            card.series_id.clone(),
                            card.episode_id.clone(),
                        ),
                        vec![card.episode_id.clone()],
                    )
                })
                .collect();
        let mut continuing = vec![first.clone(), overflow];
        let mut new = vec![(0, first_alias.clone()), (0, overflow_alias.clone())];
        deduplicate_episode_home_rows(&mut continuing, &mut new, &identities, true, 1);
        assert_eq!(continuing.len(), 1);
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].1.series_id, overflow_alias.series_id);

        let bridge = card("bridge", "bridge-episode");
        let bridged = card("bridged", "kitsu:10:1");
        for (entry, aliases) in [
            (&bridge, vec!["tt100:1:1".into(), "kitsu:10:1".into()]),
            (&bridged, vec!["kitsu:10:1".into()]),
        ] {
            identities.insert(
                (
                    entry.type_.clone(),
                    entry.series_id.clone(),
                    entry.episode_id.clone(),
                ),
                aliases,
            );
        }
        let mut continuing = vec![first.clone(), bridge];
        let mut new = vec![(0, bridged)];
        deduplicate_episode_home_rows(&mut continuing, &mut new, &identities, true, 1);
        assert!(
            new.is_empty(),
            "a suppressed alias still proves the retained episode's identity"
        );

        let mut continuing = vec![first.clone(), first];
        let mut new = vec![(0, first_alias.clone()), (0, first_alias)];
        deduplicate_episode_home_rows(&mut continuing, &mut new, &identities, false, 20);
        assert_eq!(
            continuing.len(),
            1,
            "disabled rails still deduplicate their own state"
        );
        assert_eq!(
            new.len(),
            1,
            "a disabled Continue rail cannot suppress New Episodes"
        );
    }

    #[test]
    fn continue_prefers_explicit_partial_rewatch_without_changing_completion() {
        let episodes = vec![
            video("e1", 1, 1, Some("2020-01-01")),
            video("e2", 1, 2, Some("2020-01-02")),
        ];
        let mut record = progress("rewatch", "e1", 600.0, true, 50);
        record.history = Some(PlaybackHistory {
            type_: "series".into(),
            played_at_secs: 40,
            rewatch: true,
            ..Default::default()
        });
        let mut records = HashMap::from([(progress_map_key("rewatch", "e1"), record.clone())]);
        assert_eq!(
            continue_resume_id("series", "rewatch", &record, Some(&episodes), &records),
            Some("e1".into())
        );
        assert_eq!(
            continue_badge(genuine_partial_rewatch(&record), "e1", &episodes, |_| true),
            0
        );
        assert_eq!(continue_remaining(Some(&record)), "50 min left");
        assert!(records[&progress_map_key("rewatch", "e1")].watched);
        let mut manual = progress("rewatch", "e2", 600.0, true, 100);
        manual.history = Some(PlaybackHistory {
            played_at_secs: 30,
            ..Default::default()
        });
        assert!(
            continue_playback_order(&record) > continue_playback_order(&manual),
            "manual progress changes cannot supersede a newer actual rewatch"
        );

        record.history.as_mut().unwrap().rewatch = false;
        records.insert(progress_map_key("rewatch", "e1"), record.clone());
        assert_eq!(
            continue_resume_id("series", "rewatch", &record, Some(&episodes), &records),
            Some("e2".into()),
            "a manual watched mark at an old partial position still advances"
        );
        assert_eq!(continue_remaining(Some(&record)), "Completed");
        record.history.as_mut().unwrap().rewatch = true;
        record.position_secs = f64::NAN;
        assert!(!genuine_partial_rewatch(&record));
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

    #[test]
    fn home_showcase_keeps_virtual_seasons_distinct_without_changing_original_dedup() {
        let first: MetaPreview = serde_json::from_value(serde_json::json!({
            "id":"mal:440","type":"series","name":"Combined · Season 1",
            "novaSourceUrl":"https://owner.invalid/manifest.json",
            "novaCatalogVirtualSeason":true,"novaCatalogDisplaySeason":1,
            "novaCatalogDisplayKey":"season-one"
        }))
        .unwrap();
        let mut second = first.clone();
        second.name = "Combined · Season 2".into();
        second
            .extra
            .insert("novaCatalogDisplaySeason".into(), 2.into());
        second
            .extra
            .insert("novaCatalogDisplayKey".into(), "season-two".into());
        let merged = merge_home_showcase_results(vec![
            (0, vec![first.clone(), second.clone()]),
            (1, vec![first.clone(), second.clone()]),
        ]);
        assert_eq!(
            merged.len(),
            2,
            "overlapping catalogs deduplicate the same virtual season only"
        );
        assert!(!home_showcase_same_identity(&first, &second));
        let fresh: MetaPreview = serde_json::from_value(serde_json::json!({"id":"mal:440","type":"series","name":"Fresh title","novaCatalogDisplayKey":"untrusted"})).unwrap();
        merge_home_showcase_preview(&mut second, &fresh);
        assert_eq!(second.title(), "Fresh title · Season 2");
        assert_eq!(second.extra["novaCatalogDisplayKey"], "season-two");
        let original: MetaPreview = serde_json::from_value(
            serde_json::json!({"id":"mal:440","type":"series","name":"Combined"}),
        )
        .unwrap();
        assert_eq!(
            merge_home_showcase_results(vec![(0, vec![original.clone(), original])]).len(),
            1
        );
    }
}

/// Plain row drafts cross the worker boundary; Slint images stay on the UI.
pub(super) struct PreparedProgressHome {
    continuing: Vec<ContinueRowDraft>,
    new: Vec<ContinueRowDraft>,
    upcoming: Vec<UpcomingRowDraft>,
    featured: Option<((String, String), String)>,
}

pub(super) fn prepare_progress_home(
    state: &mut Shared,
    metadata: &super::progress_projection::Metadata<'_>,
    activity: &HashMap<String, ViewingActivity>,
    new_hidden: &HashMap<String, Vec<String>>,
    groups: &super::library::LibraryDisplayGroups,
) -> PreparedProgressHome {
    const MAX: usize = 20;
    let today = today_days();
    let mut latest: HashMap<&str, &EpisodeProgress> = HashMap::new();
    for p in state.progress.values() {
        if latest
            .get(p.series_id.as_str())
            .is_none_or(|old| continue_playback_order(p) > continue_playback_order(old))
        {
            latest.insert(&p.series_id, p);
        }
    }
    let mut continuing = Vec::new();
    let mut new = Vec::new();
    let mut upcoming = Vec::new();
    let mut aliases = HashMap::new();
    for e in state.entries.iter().filter(|e| {
        profile_policy::allows_title(&profiles::ProfileStore::capture(), &e.type_, &e.id)
            && e.watch_status == WatchStatus::Auto
            && groups.is_representative(&e.type_, &e.id)
    }) {
        let cached = metadata.episode_data(&e.type_, &e.id);
        let videos = cached.as_deref().map(Vec::as_slice).unwrap_or_default();
        let watched = |v: &Video| {
            state
                .progress
                .get(&progress_map_key(&e.id, &v.id))
                .is_some_and(|p| p.watched)
        };
        if e.type_ != "movie"
            && let Some((index, air_days)) = next_upcoming_episode(videos, watched, today)
        {
            upcoming.push(UpcomingEntry {
                series_id: e.id.clone(),
                type_: e.type_.clone(),
                episode_id: videos[index].id.clone(),
                air_days,
            });
        }
        let Some(p) = latest.get(e.id.as_str()) else {
            continue;
        };
        let bucket = activity_bucket(&e.id, videos, &state.progress, activity.get(&e.id), today);
        if bucket == "Completed" {
            continue;
        }
        let returning =
            e.type_ != "movie" && bucket == "New episodes" && !genuine_partial_rewatch(p);
        let episode = if returning {
            next_episode_to_watch(&e.id, videos, &state.progress).map(|v| v.id.clone())
        } else {
            continue_resume_id(
                &e.type_,
                &e.id,
                p,
                cached.as_deref().map(Vec::as_slice),
                &state.progress,
            )
        };
        let Some(episode_id) = episode else { continue };
        let proofs = HomeEpisodeProofs::new(&e.type_, videos);
        let identity = proofs.aliases(&e.type_, &episode_id);
        if returning {
            let available = new_episode_ids(&e.id, videos, &state.progress);
            if new_episodes_are_hidden(new_hidden, &e.type_, &e.id, &available, &proofs) {
                continue;
            }
        } else if continue_is_hidden(
            &state.continue_hidden,
            &e.type_,
            &e.id,
            &identity,
            p.updated_at_secs,
        ) {
            continue;
        }
        aliases.insert(
            (e.type_.clone(), e.id.clone(), episode_id.clone()),
            identity,
        );
        let card = ContinueEntry {
            series_id: e.id.clone(),
            type_: e.type_.clone(),
            episode_id,
            updated_at_secs: p.updated_at_secs,
        };
        if returning {
            let newest = videos
                .iter()
                .filter(|v| episode_has_air_date(v) && episode_is_out(v, today) && !watched(v))
                .filter_map(|v| v.released.as_deref().and_then(iso_days))
                .max()
                .unwrap_or(0);
            new.push((newest, card));
        } else {
            continuing.push(card);
        }
    }
    new.sort_by_key(|(newest, e)| (std::cmp::Reverse(*newest), e.series_id.clone()));
    continuing.sort_by_key(|e| std::cmp::Reverse(e.updated_at_secs));
    deduplicate_episode_home_rows(
        &mut continuing,
        &mut new,
        &aliases,
        state
            .cache_settings
            .home_row_enabled(&HomeRowSource::ContinueWatching),
        MAX,
    );
    upcoming.sort_by_key(|e| e.air_days);
    upcoming.truncate(MAX);
    state.continue_list = continuing;
    state.new_episodes_list = new.into_iter().map(|(_, e)| e).collect();
    state.upcoming_list = upcoming;
    let episode_rows = |list: &[ContinueEntry], new: bool| {
        if !state.cache_settings.home_row_enabled(if new {
            &HomeRowSource::NewEpisodes
        } else {
            &HomeRowSource::ContinueWatching
        }) {
            return Vec::new();
        }
        list.iter()
            .filter_map(|c| {
                let e = state
                    .entries
                    .iter()
                    .find(|e| e.id == c.series_id && e.type_ == c.type_)?;
                let cached = metadata.episodes(&c.type_, &c.series_id);
                let videos = cached.as_slice();
                let video = videos.iter().find(|v| v.id == c.episode_id);
                let record = state
                    .progress
                    .get(&progress_map_key(&c.series_id, &c.episode_id));
                let started = record.is_some_and(|p| {
                    (!p.watched && p.position_secs > 0.0) || genuine_partial_rewatch(p)
                });
                let badge = continue_badge(started, &c.episode_id, videos, |v| {
                    state
                        .progress
                        .get(&progress_map_key(&c.series_id, &v.id))
                        .is_some_and(|p| p.watched)
                });
                Some(ContinueRowDraft {
                    id: c.series_id.clone(),
                    type_: c.type_.clone(),
                    episode_id: c.episode_id.clone(),
                    title: e.name.clone(),
                    subtitle: video
                        .map(|v| format!("{} · {}", episode_badge(v), episode_row_label(v)))
                        .unwrap_or_else(|| text::tr("Resume").into()),
                    episode_title: video.map(episode_row_label).unwrap_or_default(),
                    ep_no: video.map(episode_badge).unwrap_or_default(),
                    remaining: if new {
                        text::new_episodes(
                            new_episode_ids(&c.series_id, videos, &state.progress).len(),
                        )
                    } else {
                        continue_remaining(record)
                    },
                    art_url: home_card_art_url(
                        &e.poster_url,
                        video,
                        state.cache_settings.home_episode_artwork,
                    ),
                    poster_url: e.poster_url.clone(),
                    progress: if new {
                        0.0
                    } else {
                        record
                            .map(|p| progress_fraction(p.position_secs, p.duration_secs))
                            .unwrap_or_default()
                    },
                    badge: if new {
                        2
                    } else if badge == 2 {
                        1
                    } else {
                        badge
                    },
                })
            })
            .collect()
    };
    let upcoming = if state
        .cache_settings
        .home_row_enabled(&HomeRowSource::Upcoming)
    {
        state
            .upcoming_list
            .iter()
            .enumerate()
            .filter_map(|(index, u)| {
                let e = state
                    .entries
                    .iter()
                    .find(|e| e.id == u.series_id && e.type_ == u.type_)?;
                let cached = metadata.episodes(&u.type_, &u.series_id);
                let video = cached.iter().find(|v| v.id == u.episode_id)?;
                Some(UpcomingRowDraft {
                    id: u.series_id.clone(),
                    title: e.name.clone(),
                    subtitle: format!("{} · {}", episode_badge(video), episode_row_label(video)),
                    date: video
                        .released
                        .as_deref()
                        .and_then(|date| {
                            Bridge::format_human_date(date, state.cache_settings.date_relative)
                        })
                        .unwrap_or_default(),
                    episode_title: episode_row_label(video),
                    ep_no: episode_badge(video),
                    art_url: home_card_art_url(
                        &e.poster_url,
                        Some(video),
                        state.cache_settings.home_episode_artwork,
                    ),
                    poster_url: e.poster_url.clone(),
                    index,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    let featured = state.home_showcase_displayed.as_ref().map(|p| {
        let episodes = (p.type_ != "movie")
            .then(|| metadata.episode_data(&p.type_, &p.id))
            .flatten();
        (
            (p.type_.clone(), p.id.clone()),
            watch_action_label(
                &p.id,
                episodes.as_deref().map(Vec::as_slice),
                &state.progress,
            ),
        )
    });
    PreparedProgressHome {
        continuing: episode_rows(&state.continue_list, false),
        new: episode_rows(&state.new_episodes_list, true),
        upcoming,
        featured,
    }
}

impl Bridge {
    pub(super) fn publish_progress_home(&self, mut prepared: PreparedProgressHome) {
        let Some(app) = self.app() else { return };
        {
            let mut state = self.shared.lock().unwrap();
            prune_watched_new_episode_rows(&mut state);
            prepared.new.retain(|draft| {
                state.new_episodes_list.iter().any(|row| {
                    row.series_id == draft.id
                        && row.type_ == draft.type_
                        && row.episode_id == draft.episode_id
                })
            });
        }
        let old_continue = app.get_home_continue();
        let old_new = app.get_home_new_episodes();
        let old_upcoming = app.get_home_upcoming();
        let continuation_rows = |drafts: Vec<ContinueRowDraft>,
                                 old: slint::ModelRc<ContinueRow>| {
            drafts
                .into_iter()
                .map(|d| {
                    let old = old.iter().find(|row| {
                        row.id.as_str() == d.id
                            && row.art_url.as_str() == d.art_url
                            && row.is_loaded
                    });
                    let (poster, is_loaded) = old
                        .map(|row| (row.poster, true))
                        .unwrap_or_else(|| home_card_image(&d.art_url, &d.poster_url));
                    ContinueRow {
                        media_type: d.type_.into(),
                        episode_id: d.episode_id.into(),
                        id: d.id.into(),
                        title: d.title.into(),
                        subtitle: d.subtitle.into(),
                        episode_title: d.episode_title.into(),
                        ep_no: d.ep_no.into(),
                        remaining: d.remaining.into(),
                        art_url: d.art_url.into(),
                        poster,
                        is_loaded,
                        progress: d.progress,
                        badge: d.badge,
                    }
                })
                .collect::<Vec<_>>()
        };
        app.set_home_continue(
            Rc::new(VecModel::from(continuation_rows(
                prepared.continuing,
                old_continue,
            )))
            .into(),
        );
        app.set_home_new_episodes(
            Rc::new(VecModel::from(continuation_rows(prepared.new, old_new))).into(),
        );
        let upcoming = prepared
            .upcoming
            .into_iter()
            .map(|d| {
                let old = old_upcoming.iter().find(|row| {
                    row.id.as_str() == d.id && row.art_url.as_str() == d.art_url && row.is_loaded
                });
                let (poster, is_loaded) = old
                    .map(|row| (row.poster, true))
                    .unwrap_or_else(|| home_card_image(&d.art_url, &d.poster_url));
                UpcomingRow {
                    id: d.id.into(),
                    title: d.title.into(),
                    subtitle: d.subtitle.into(),
                    date: d.date.into(),
                    episode_title: d.episode_title.into(),
                    ep_no: d.ep_no.into(),
                    art_url: d.art_url.into(),
                    poster,
                    is_loaded,
                    index: d.index as i32,
                }
            })
            .collect::<Vec<_>>();
        app.set_home_upcoming(Rc::new(VecModel::from(upcoming.clone())).into());
        // Heal current UI-only calendar selection, rather than replaying the
        // selection that was current when the worker started.
        let (cal, cells) = {
            let mut state = self.shared.lock().unwrap();
            heal_upcoming_cal(&mut state);
            let cal = state.upcoming_cal.clone();
            let mut counts = HashMap::new();
            for u in &state.upcoming_list {
                *counts.entry(u.air_days).or_insert(0) += 1;
            }
            let cells = cal_cells(cal.first, &counts, cal.day, today_days());
            (cal, cells)
        };
        let days = self
            .shared
            .lock()
            .unwrap()
            .upcoming_list
            .iter()
            .enumerate()
            .filter(|(_, u)| Some(u.air_days) == cal.day)
            .map(|(i, _)| i as i32)
            .collect::<HashSet<_>>();
        app.set_home_cal_day(
            Rc::new(VecModel::from(
                upcoming
                    .into_iter()
                    .filter(|row| days.contains(&row.index))
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        app.set_home_cal_cells(Rc::new(VecModel::from(cells)).into());
        let (year, month, _) = civil_from_days(cal.first);
        app.set_home_cal_title(text::cal_month_title(month, year).into());
        app.set_home_cal_epoch(cal.day.unwrap_or(-1) as i32);
        app.set_home_cal_open(cal.open);
        if let Some((identity, label)) = prepared.featured
            && self
                .shared
                .lock()
                .unwrap()
                .home_showcase_displayed
                .as_ref()
                .is_some_and(|p| p.type_ == identity.0 && p.id == identity.1)
        {
            app.set_home_featured_watch_label(label.into());
        }
        let (groups, settings) = {
            let state = self.shared.lock().unwrap();
            (
                state.home_catalog_row_groups.clone(),
                state.cache_settings.clone(),
            )
        };
        if (app.get_home_view() == 1
            && !settings.home_row_enabled(&HomeRowSource::ContinueWatching))
            || (app.get_home_view() == 2 && !settings.home_row_enabled(&HomeRowSource::Upcoming))
            || (app.get_home_view() == -3
                && !settings.home_row_enabled(&HomeRowSource::NewEpisodes))
        {
            app.set_home_view(0);
        }
        self.install_prepared_home_catalog_rows(groups);
        self.dispatch_continue_posters();
        self.dispatch_upcoming_posters();
    }
}

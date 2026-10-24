//! Detail modal: metadata, seasons/episodes, stream list.
use super::*;
use std::time::Instant;

const STREAM_PAGE_SIZE: usize = 25;
const STREAM_REFRESH_COOLDOWN: Duration = Duration::from_secs(2);

/// History anchors use the latest observed episode, including completed ones,
/// rather than the next unwatched episode advertised by Continue Watching.
fn last_watched_episode<'a>(state: &Shared, modal: &'a ModalItem) -> Option<&'a Video> {
    modal
        .videos
        .iter()
        .filter(|video| catalog_detail::allows(state, modal, &video.id))
        .filter_map(|video| {
            let progress = state
                .progress
                .get(&progress_map_key(&modal.id, &video.id))?;
            (video.season.is_some()
                && (progress.position_secs > 0.0 || progress.watched || progress.play_count > 0))
                .then_some((video, progress))
        })
        .max_by_key(|(video, progress)| (progress.updated_at_secs, video.id.as_str()))
        .map(|(video, _)| video)
}

static WATCH_ACTION_BOOT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{:032x}-{:08x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id()
    )
});
static WATCH_ACTION_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static WATCH_ACTION_DEVICE: AtomicU64 = AtomicU64::new(0);

pub(super) fn initialize_manual_watch_actions(device: u64) {
    WATCH_ACTION_DEVICE.store(device, Ordering::Relaxed);
}

fn next_manual_watch_action() -> String {
    format!(
        "{:016x}-{}-{:016x}",
        WATCH_ACTION_DEVICE.load(Ordering::Relaxed),
        *WATCH_ACTION_BOOT,
        WATCH_ACTION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// The backend guards every activation, including keyboard repeats and a
/// close/reopen of the selector. UI feedback is only a reflection of this gate.
#[derive(Default)]
pub(super) struct StreamRefreshGate {
    next: Option<Instant>,
    timer_epoch: Option<u64>,
    timer_generation: u64,
}

impl StreamRefreshGate {
    fn ready(&self, now: Instant) -> bool {
        self.next.is_none_or(|next| now >= next)
    }

    fn claim(&mut self, now: Instant, busy: bool) -> bool {
        if busy || !self.ready(now) {
            return false;
        }
        self.next = Some(now + STREAM_REFRESH_COOLDOWN);
        self.timer_epoch = None;
        true
    }

    fn publication(&mut self, now: Instant, epoch: u64) -> (bool, Option<(Duration, u64)>) {
        let ready = self.ready(now);
        let timer = if !ready && self.timer_epoch != Some(epoch) {
            self.timer_epoch = Some(epoch);
            self.timer_generation = self.timer_generation.wrapping_add(1);
            self.next
                .map(|next| (next.saturating_duration_since(now), self.timer_generation))
        } else {
            None
        };
        (ready, timer)
    }

    fn wake(&mut self, generation: u64) -> bool {
        if self.timer_generation != generation {
            return false;
        }
        self.timer_epoch = None;
        true
    }
}

#[derive(Clone)]
struct StreamResponseContext {
    request_id: String,
    media_type: String,
    generation: u64,
    addon: String,
    owner_url: String,
    owner_generation: u64,
    settings_generation: u64,
    tracks_settings: bool,
    cache_hit: bool,
}

/// Number of pages a season of `total` filtered episodes needs (always ≥ 1).
pub(crate) fn page_count(total: usize) -> usize {
    total.div_ceil(EPISODE_PAGE_SIZE).max(1)
}

/// A preview cannot exceed the player's selected maximum even when its list
/// thumbnail was decoded at a larger resolution.
fn player_episode_preview(
    pixels: &SharedPixelBuffer<Rgba8Pixel>,
    settings: &CacheSettings,
) -> SharedPixelBuffer<Rgba8Pixel> {
    match ImageRecipe::resolve(ImageUse::PlayerEpisodeBackdrop, settings).max_side {
        Some(side) => downscale_for_display(pixels, side),
        None => pixels.clone(),
    }
}

// A cached list image is only a provisional player loading backdrop. A
// delayed resize must never replace a newer recipe fetch or another selection.
struct EpisodePreviewRequest {
    open_token: Arc<()>,
    episode_id: String,
    stream_generation: u64,
    image_revision: u64,
}
impl EpisodePreviewRequest {
    fn accepts(&self, state: &Shared, revision: u64, has_backdrop: bool) -> bool {
        !has_backdrop
            && revision == self.image_revision
            && state.stream_generation == self.stream_generation
            && state.modal_item.as_ref().is_some_and(|modal| {
                Arc::ptr_eq(&modal.open_token, &self.open_token)
                    && modal.request_id == self.episode_id
            })
    }
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
        if !searching && self.open_catalog_group(&preview, generation, discover_index, watch_now) {
            return;
        }
        let preview = self.catalog_grouping_source_preview(preview);
        self.shared.lock().unwrap().related_detail_history.clear();
        self.open_preview_inner(
            preview,
            generation,
            searching,
            discover_index,
            watch_now,
            true,
        );
    }

    pub(super) fn open_preview_inner(
        &self,
        preview: MetaPreview,
        generation: u64,
        searching: bool,
        discover_index: Option<usize>,
        watch_now: bool,
        record_source: bool,
    ) {
        if !profile_policy::allows_browse_title(
            &profiles::ProfileStore::capture(),
            &preview.type_,
            &preview.id,
        ) {
            return;
        }
        let catalog_opening = self.begin_catalog_detail_open();
        self.cancel_season_download();
        let _ = (generation, searching);
        let app = match self.app() {
            Some(a) => a,
            None => return,
        };

        let content = (*app).global::<nova_ui_shared::DetailContent>();
        content.set_restoring_navigation(!record_source);
        if record_source {
            content.set_synopsis_expanded(false);
            content.set_hero_expanded(false);
            content.set_trailers_expanded(false);
        }

        // A Back restoration reconstructs presentation fields, not a fresh
        // provider identity observation. Retain its existing tracker evidence.
        if record_source {
            tracking::remember_source(&preview);
        }
        if app.get_tracking_open() {
            self.tracking_close();
        }
        {
            let mut state = self.shared.lock().unwrap();
            state.history_selection = None;
            state.modal_item = Some(ModalItem {
                details: detail_extras::DetailExtras::from_item(&MetaItem {
                    preview: preview.clone(),
                    ..Default::default()
                }),
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
                season_backdrops: HashMap::new(),
                seasons: Vec::new(),
                season_index: 0,
                episode_page: 0,
                name: preview.title(),
                year: preview.year_str().unwrap_or_default(),
                poster_url: preview.poster.clone().unwrap_or_default(),
                background_url: preview.background.clone().unwrap_or_default(),
                logo_url: preview.logo.clone().unwrap_or_default(),
                description: preview.description.clone().unwrap_or_default(),
                genres: preview.genres.clone(),
            });
            state.streams.clear();
        }

        self.bind_catalog_detail_open(catalog_opening);

        // The last resolved detail text takes precedence over a thinner
        // catalog preview, so reentry does not replay the same corrections.
        // Missing cached fields still fall back to the preview.
        self.restore_modal_header_from_cache(&preview.id);
        self.publish_detail_extras();
        let (paint_description, paint_genres, paint_year, is_movie) = {
            let state = self.shared.lock().unwrap();
            match state.modal_item.as_ref() {
                Some(m) => (
                    m.description.clone(),
                    m.genres.clone(),
                    m.year.clone(),
                    m.type_ == "movie",
                ),
                None => return,
            }
        };

        app.set_selected_title(SharedString::from(preview.title()));
        app.set_detail_is_movie(is_movie);
        self.refresh_smart_offline();
        app.set_stream_selector_open(false);
        app.set_detail_episode_reveal_request(0);
        app.set_selected_year(SharedString::from(&paint_year));
        app.set_selected_index(discover_index.map(|i| i as i32).unwrap_or(-1));
        if let Some(index) = discover_index {
            // Grid return target: backing out re-focuses the opened card (the
            // lifted kb props survive the grid page's recreation).
            app.set_discover_kb_zone(3);
            app.set_discover_kb_idx(index as i32);
        }
        self.clear_streams();
        // Reset any leftover episode-picker state from a previous item.
        app.set_modal_episodes(false);
        app.set_detail_deep_stream(false);
        apply_stream_episode_context(&app, None);
        app.set_season_names(Rc::new(VecModel::<SharedString>::from(vec![])).into());
        app.set_season_combo_idx(-1);
        app.set_season_cards(Rc::new(VecModel::<SeasonCard>::from(vec![])).into());
        app.set_episode_rows(Rc::new(VecModel::<EpisodeRow>::from(vec![])).into());
        // Paint the resolved header before mounting; fresh metadata can
        // still fill gaps or publish real upstream changes in the background.
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
        app.set_episode_filter(SharedString::default());

        // Detail always resolves its own recipe: a grid-sized completion must
        // not become the selected image when Detail requests another size.
        app.set_selected_poster(Image::default());
        app.set_player_poster(Image::default());

        app.set_in_library(self.library_contains(&preview.type_, &preview.id));
        self.refresh_detail_library_watched();
        if self.library_contains(&preview.type_, &preview.id) {
            self.sync_modal_category_flags();
        }

        let (item_id, modal_type, poster_url, backdrop_url, header) = {
            let state = self.shared.lock().unwrap();
            let m = state.modal_item.as_ref().unwrap();
            (
                m.id.clone(),
                m.type_.clone(),
                m.poster_url.clone(),
                m.background_url.clone(),
                (m.genres.clone(), m.description.clone(), m.year.clone()),
            )
        };
        if app.get_selected_poster().size().width == 0 && !poster_url.is_empty() {
            // Search/featured entries and Android do not share the desktop
            // browse worker's fast path. Reuse/decode their cached poster too.
            self.load_detail_poster(poster_url, item_id.clone());
        }
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
        self.load_current_detail_logo();
        // Backfill the persisted library URL so library reopens (and
        // restarts) can start the image load without a meta fetch.
        self.persist_backdrop_for(&item_id, &backdrop_url);
        // Backfill persisted header text for the same reason.
        self.persist_header_for(&item_id, &header.0, &header.1, &header.2, false);
        // Configure the incoming shell before it can render. Metadata must
        // not swap Overview for Episodes after the entrance has begun.
        self.restore_detail_snapshot(&item_id);

        if modal_type == "movie" {
            self.cancel_watch_now();
            // Movies keep the direct behaviour: fetch streams right away.
            self.start_stream_search(item_id.clone());
            self.refresh_movie_meta(item_id);
            self.refresh_watch_action();
            if watch_now {
                app.set_stream_selector_open(true);
                app.set_detail_deep_stream(true);
            }
        } else {
            // Series-like: list seasons/episodes first and only fetch
            // streams once an episode is picked.
            self.prepare_episodes(item_id, modal_type);
        }
        // Cached episodes and the primary action must also be ready before
        // constructing the incoming shell (including featured shortcuts).
        self.load_detail_similar();
        app.set_modal_visible(true);
    }

    /// Movies also recheck metadata behind cached content; stream discovery
    /// remains independent so refreshing artwork never delays playback.
    pub(super) fn refresh_movie_meta(&self, id: String) {
        let (open_token, urls) = {
            let state = self.shared.lock().unwrap();
            let Some(item) = state
                .modal_item
                .as_ref()
                .filter(|item| item.id == id && item.type_ == "movie")
            else {
                return;
            };
            let urls = state
                .installed
                .iter()
                .filter(|addon| {
                    addon.enabled && addon.available && addon.manifest.accepts("meta", "movie", &id)
                })
                .filter_map(|addon| Addon::new(&addon.url).ok())
                .map(|addon| addon.meta_url("movie", &id))
                .collect::<VecDeque<_>>();
            (item.open_token.clone(), urls)
        };
        fn next(bridge: Bridge, id: String, open_token: Arc<()>, mut urls: VecDeque<String>) {
            let Some(url) = urls.pop_front() else { return };
            net::fetch_bytes(url, move |result| {
                let found = result
                    .ok()
                    .and_then(|bytes| Addon::parse_meta(&bytes).ok().flatten())
                    .filter(|item| meta_matches_request(item, "movie", &id));
                let Some(item) = found else {
                    next(bridge, id, open_token, urls);
                    return;
                };
                bridge.cache_background_meta("movie", &id, &item);
                let _ = crate::app::profiles::invoke_from_event_loop(move || {
                    let still_open = bridge
                        .shared
                        .lock()
                        .unwrap()
                        .modal_item
                        .as_ref()
                        .is_some_and(|modal| {
                            modal.id == id
                                && modal.type_ == "movie"
                                && Arc::ptr_eq(&modal.open_token, &open_token)
                        });
                    if still_open && bridge.app().is_some_and(|app| app.get_modal_visible()) {
                        bridge.upgrade_detail_meta(&id, &item, true, true);
                    } else if let Some(poster) = item
                        .preview
                        .poster
                        .as_ref()
                        .filter(|url| !url.trim().is_empty())
                    {
                        bridge.load_discover_poster("movie".into(), id.clone(), poster.clone());
                    }
                });
            });
        }
        next(self.clone(), id, open_token, urls);
    }

    /// Fetch and display streams for `request_id` (a movie id, or the
    /// `<item>:<season>:<episode>` id of a picked episode) from every
    /// installed addon that provides streams for the modal item's type.
    pub(super) fn start_stream_search(&self, request_id: String) {
        self.start_stream_search_inner(request_id, false);
    }

    /// Explicit refresh preserves the selected addon while replacing every
    /// source response. Navigation continues to start from All.
    pub(super) fn start_stream_search_inner(&self, request_id: String, preserve_filter: bool) {
        if profile_policy::entry_blocked() {
            return;
        }
        self.publish_stream_refresh_gate();
        if self.remote_output_selected() {
            if let Some(modal) = self.shared.lock().unwrap().modal_item.as_mut() {
                modal.request_id = request_id.clone();
            }
            if self.remote_reload_streams() {
                return;
            }
        }
        let changed = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .is_some_and(|modal| modal.request_id != request_id);
        if changed {
            self.cancel_pending_for_navigation();
        }
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
        let stream_urls = {
            let state = self.shared.lock().unwrap();
            let mut urls = stream_search_urls_for(&state, &request_id);
            if state.season_download.is_some() {
                urls.truncate(16);
            }
            urls
        };

        let mut seen_labels = HashSet::new();
        let requested: Vec<String> = stream_urls
            .iter()
            .map(|(label, ..)| label.clone())
            .filter(|label| seen_labels.insert(label.clone()))
            .collect();
        let addon_count = requested.len();
        // Show one pill per addon being queried right away (spinner), then
        // drop each one that answers with no streams.
        let (generation, filter) = {
            let mut state = self.shared.lock().unwrap();
            let selected = preserve_filter
                .then(|| {
                    state
                        .stream_filter
                        .checked_sub(1)
                        .and_then(|index| state.stream_addons.get(index))
                        .cloned()
                })
                .flatten();
            if let Some(m) = state.modal_item.as_mut() {
                m.request_id = request_id.clone();
            }
            state.stream_generation = state.stream_generation.wrapping_add(1);
            state.stream_revealed_ids.clear();
            state.streams.clear();
            state.stream_all.clear();
            state.stream_addons = requested.clone();
            state.stream_pending = stream_urls
                .iter()
                .map(|(_, _, owner, _)| owner.clone())
                .collect();
            state.stream_owner_order = state.stream_pending.clone();
            state.stream_cached_owners.clear();
            state.stream_owner_labels = stream_urls
                .iter()
                .map(|(label, _, owner, _)| (owner.clone(), label.clone()))
                .collect();
            state.stream_warnings.clear();
            state.stream_filter = selected
                .as_ref()
                .and_then(|label| requested.iter().position(|candidate| candidate == label))
                .map_or(0, |index| index + 1);
            state.stream_page = 0;
            (state.stream_generation, state.stream_filter)
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
            app.set_stream_filter(filter as i32);
        }
        // Settings-driven refetches also need immediate in-flight feedback;
        // the entry publication above still saw the previous pending state.
        self.publish_stream_refresh_gate();
        // Show already-downloaded rows for this request immediately, before
        // the first addon answers. Addon streams keep streaming in per addon
        // (see `stream_addon_updated`), pinned below the downloads.
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
        let limit = if self.shared.lock().unwrap().season_download.is_some() {
            16
        } else {
            usize::MAX
        };
        for (addon, url, owner_url, owner_generation) in stream_urls.into_iter().take(limit) {
            let context = StreamResponseContext {
                request_id: request_id.clone(),
                media_type: modal_type.clone(),
                generation,
                addon,
                tracks_settings: nova_providers::provider_settings_schema(&owner_url)
                    .is_some_and(|schema| !schema.is_empty()),
                owner_url,
                owner_generation,
                settings_generation: nova_providers::plugin_settings::settings_generation(),
                cache_hit: false,
            };
            let partial_bridge = self.clone();
            let partial_context = context.clone();
            let final_bridge = self.clone();
            self.request_streams_with_prefetch_cache(
                url,
                context.owner_url.clone(),
                context.owner_generation,
                context.settings_generation,
                Arc::new(move |bytes| {
                    partial_bridge.receive_stream_response(
                        partial_context.clone(),
                        Ok(bytes),
                        false,
                    );
                }),
                move |result| {
                    let mut context = context;
                    let result = result.map(|response| {
                        context.cache_hit = response.cache_hit;
                        response.bytes
                    });
                    final_bridge.receive_stream_response(context, result, true);
                },
            );
        }
    }

    pub(super) fn refresh_stream_sources(&self) {
        let Some(app) = self.app() else { return };
        if !app.get_modal_visible()
            || !app.get_stream_selector_open()
            || app.get_player_open()
            || app.get_season_download_review()
        {
            return;
        }
        let request = {
            let mut state = self.shared.lock().unwrap();
            let Some(request) = state
                .modal_item
                .as_ref()
                .map(|modal| modal.request_id.clone())
                .filter(|id| !id.is_empty())
            else {
                return;
            };
            let busy = app.get_streams_searching()
                || (app.get_remote_target_id().is_empty() && !state.stream_pending.is_empty());
            if !state.stream_refresh_gate.claim(Instant::now(), busy) {
                return;
            }
            // A new cache epoch neither joins an old speculative request nor
            // lets its late completion overwrite a manually refreshed result.
            state.stream_prefetch.invalidate();
            request
        };
        app.set_stream_action_open(false);
        app.set_detail_kb_s(0);
        self.start_stream_search_inner(request, true);
    }

    fn publish_stream_refresh_gate(&self) {
        let Some(app) = self.app() else { return };
        let context = profiles::capture();
        let (ready, timer) = {
            let mut state = self.shared.lock().unwrap();
            state
                .stream_refresh_gate
                .publication(Instant::now(), context.epoch)
        };
        app.set_stream_refresh_enabled(ready && !app.get_streams_searching());
        if let Some((wait, generation)) = timer {
            let bridge = self.clone();
            slint::Timer::single_shot(wait, move || {
                if profiles::is_current(&context) {
                    profiles::with_context(context, || {
                        {
                            let mut state = bridge.shared.lock().unwrap();
                            // A native timer can wake just before its deadline.
                            // Re-arm instead of leaving the button disabled.
                            if !state.stream_refresh_gate.wake(generation) {
                                return;
                            }
                        }
                        bridge.publish_stream_refresh_gate();
                    });
                }
            });
        }
    }

    /// Each provider publication is a replacement snapshot. Decode on the
    /// worker; only the event-loop closure can mutate the active picker.
    fn receive_stream_response(
        &self,
        mut context: StreamResponseContext,
        result: Result<Vec<u8>, net::FetchError>,
        complete: bool,
    ) {
        let warnings = result
            .as_ref()
            .ok()
            .map(|bytes| provider_stream_warnings(bytes, &context.owner_url))
            .unwrap_or_else(|| {
                if context.tracks_settings {
                    vec![(
                        context.addon.clone(),
                        "Provider unavailable. Try again later.".into(),
                    )]
                } else {
                    vec![]
                }
            });
        let failed = result.is_err();
        let mut ids = HashSet::new();
        let new_rows = result
            .ok()
            .and_then(|bytes| Addon::parse_streams(&bytes).ok())
            .map(|streams| {
                streams
                    .into_iter()
                    .take(80)
                    .filter_map(|stream| {
                        let id = format!(
                            "{}:{}",
                            context.generation,
                            nova_providers::stable_stream_identity(
                                &context.owner_url,
                                &context.request_id,
                                &stream
                            )
                        );
                        if !ids.insert(id.clone()) {
                            return None;
                        }
                        Some(StreamUi {
                            id,
                            choice_id: Some(stream_choice_identity(
                                &context.owner_url,
                                &context.media_type,
                                &context.request_id,
                                &stream,
                            )),
                            owner_url: context.owner_url.clone(),
                            display: stream_display(&stream),
                            card: stream_card_text(&stream),
                            source: stream_source_for_addon(
                                &stream,
                                &context.owner_url,
                                context.owner_generation,
                                context.settings_generation,
                            ),
                            addon: context.addon.clone(),
                            download: None,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let bridge = self.clone();
        let _ = crate::app::profiles::invoke_from_event_loop(move || {
            let stale = {
                let state = bridge.shared.lock().unwrap();
                !state.installed.iter().any(|addon| {
                    addon.url == context.owner_url
                        && addon.enabled
                        && addon.available
                        && addon.generation == context.owner_generation
                }) || (context.tracks_settings
                    && context.settings_generation
                        != nova_providers::plugin_settings::settings_generation())
            };
            if stale && !complete {
                return;
            }
            let rows = {
                let mut state = bridge.shared.lock().unwrap();
                if !stream_response_is_current(&state, &context.request_id, context.generation) {
                    return;
                }
                let displayed_warnings = if stale {
                    vec![(
                        context.addon.clone(),
                        "This stream selection is out of date. Reload streams.".into(),
                    )]
                } else if failed
                    && state
                        .stream_all
                        .iter()
                        .any(|row| row.owner_url == context.owner_url)
                {
                    // Valid progressive results remain usable after a late
                    // timeout; the provider is not unavailable to this picker.
                    vec![]
                } else {
                    warnings
                };
                state
                    .stream_warnings
                    .insert(context.owner_url.clone(), displayed_warnings);
                if stale {
                    context.cache_hit = false;
                    vec![]
                } else if failed {
                    // A late network failure cannot erase already validated
                    // partial results from the same unchanged operation.
                    state
                        .stream_all
                        .iter()
                        .filter(|row| row.owner_url == context.owner_url)
                        .cloned()
                        .collect()
                } else {
                    new_rows
                }
            };
            bridge.stream_addon_updated(context, rows, complete);
        });
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
        self.refresh_watch_action();
        // Fast path: if this item's episode list was fetched before, show the
        // cached version instantly while the background refresh below runs.
        let had_cache = if let Some(cached) = read_episodes_cache_for(&modal_type, &id)
            .filter(|videos| !ordered_seasons(videos).is_empty())
        {
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
            let owner = state
                .modal_item
                .as_ref()
                .and_then(|m| state.catalog_detail.owner(m));
            state
                .installed
                .iter()
                .filter(|a| {
                    a.enabled
                        && a.available
                        && a.manifest.accepts("meta", &modal_type, &id)
                        && owner.is_none_or(|(source, generation)| {
                            a.url == source.addon_url && Some(a.generation) == generation
                        })
                })
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
        let scoped = self.shared.lock().unwrap().catalog_detail.active();
        let bridge = self.clone();
        // Meta endpoint URLs built on the main thread; addons are tried in
        // order via fetch-continuations until one lists episodes.
        let meta_urls = candidates
            .iter()
            .filter_map(|installed| {
                let addon = Addon::new(&installed.url).ok()?;
                Some((
                    nova_providers::catalog_grouping::SourceIdentity {
                        addon_url: installed.url.clone(),
                        media_type: modal_type.clone(),
                        id: id.clone(),
                    },
                    installed.generation,
                    addon.meta_url(&modal_type, &id),
                ))
            })
            .collect::<VecDeque<_>>();
        #[allow(clippy::too_many_arguments)]
        fn run(
            bridge: Bridge,
            modal_type: String,
            id: String,
            refresh: bool,
            open_token: Arc<()>,
            mut urls: VecDeque<(
                nova_providers::catalog_grouping::SourceIdentity,
                u64,
                String,
            )>,
            profile: profiles::Context,
            revision: u64,
            scoped: bool,
        ) {
            let Some((source, owner_generation, url)) = urls.pop_front() else {
                let _ = profiles::invoke_from_event_loop(move || {
                    bridge.apply_episode_meta_owned(id, None, refresh, open_token, None)
                });
                return;
            };
            profiles::with_context(profile.clone(), || {
                net::fetch_bytes(url, move |result| {
                    profiles::with_context(profile.clone(), || {
                        let found = result
                            .ok()
                            .and_then(|bytes| Addon::parse_meta(&bytes).ok().flatten())
                            .filter(|item| {
                                meta_matches_request(item, &modal_type, &id)
                                    && (item.videos.iter().any(|v| v.season.is_some())
                                        || (!item.videos.is_empty() && scoped))
                            });
                        let Some(mut item) = found else {
                            run(
                                bridge, modal_type, id, refresh, open_token, urls, profile,
                                revision, scoped,
                            );
                            return;
                        };
                        if !profiles::is_current(&profile) {
                            return;
                        }
                        let owner_current =
                            bridge.shared.lock().unwrap().installed.iter().any(|a| {
                                a.url == source.addon_url
                                    && a.enabled
                                    && a.available
                                    && a.generation == owner_generation
                            });
                        if revision != nova_providers::metadata_revision() || !owner_current {
                            let receipt = catalog_detail::Receipt {
                                source,
                                owner_generation,
                                revision,
                                evidence: None,
                            };
                            let _ = profiles::invoke_from_event_loop(move || {
                                bridge.apply_episode_meta_owned(
                                    id,
                                    None,
                                    refresh,
                                    open_token,
                                    Some(receipt),
                                )
                            });
                            return;
                        }
                        let evidence = profiles::with_context(profile.clone(), || {
                            let cached = nova_providers::catalog_grouping::cached_evidence(
                                &source, revision,
                            )
                            .flatten();
                            // A same-ID inventory alone cannot revalidate old
                            // canonical triples. The provider verdict separates
                            // a legitimate external broker proof from removed,
                            // partial or contradictory owner mapping fields.
                            let proof =
                                nova_providers::catalog_grouping::evidence_for_refreshed_detail(
                                    &source,
                                    &item,
                                    &source.addon_url,
                                    cached,
                                );
                            // Evidence persistence stays on this worker. A changed or
                            // newly unmapped inventory clears obsolete catalog proof.
                            if revision == nova_providers::metadata_revision() {
                                nova_providers::catalog_grouping::remember_evidence(
                                    &source,
                                    revision,
                                    proof.clone(),
                                );
                            }
                            // A virtual view is bound to this exact owner's complete
                            // native inventory, not a merged/sliced canonical cache.
                            // Keep even episodes without native season numbers.
                            item.videos = if scoped {
                                write_episodes_cache_for(&modal_type, &id, &item.videos)
                            } else {
                                write_episode_meta_cache_for(&modal_type, &id, &item)
                            };
                            proof
                        });
                        let receipt = catalog_detail::Receipt {
                            source,
                            owner_generation,
                            revision,
                            evidence,
                        };
                        let _ = profiles::invoke_from_event_loop(move || {
                            bridge.apply_episode_meta_owned(
                                id,
                                Some(item),
                                refresh,
                                open_token,
                                Some(receipt),
                            )
                        });
                    })
                })
            });
        }
        run(
            bridge,
            modal_type,
            id,
            refresh,
            open_token,
            meta_urls,
            profiles::capture(),
            nova_providers::metadata_revision(),
            scoped,
        );
    }

    /// Restore the last resolved detail text over catalog/library snapshots.
    /// Cached artwork fills missing URLs; empty cache fields never erase
    /// usable preview data. Callers paint the result before mounting.
    pub(super) fn restore_modal_header_from_cache(&self, id: &str) -> bool {
        let (type_, token) = {
            let state = self.shared.lock().unwrap();
            let m = match state.modal_item.as_ref() {
                Some(m) if m.id == id => m,
                _ => return false,
            };
            (m.type_.clone(), m.open_token.clone())
        };
        let cached = read_meta_header_for(&type_, id);
        let Some(cached) = cached else {
            return false;
        };
        let mut state = self.shared.lock().unwrap();
        let m = match state.modal_item.as_mut() {
            Some(m) if m.id == id && m.type_ == type_ && Arc::ptr_eq(&m.open_token, &token) => m,
            _ => return false,
        };
        let mut touched = false;
        touched |= m.details.merge(&cached.details);
        if m.poster_url.is_empty() && !cached.poster_url.is_empty() {
            m.poster_url = cached.poster_url.clone();
            touched = true;
        }
        for (season, url) in cached.season_backdrops {
            if let std::collections::hash_map::Entry::Vacant(entry) =
                m.season_backdrops.entry(season)
            {
                entry.insert(url);
                touched = true;
            }
        }
        if m.background_url.is_empty() && !cached.background_url.is_empty() {
            m.background_url = cached.background_url.clone();
            touched = true;
        }
        if m.logo_url.is_empty() && !cached.logo_url.is_empty() {
            m.logo_url = cached.logo_url.clone();
            touched = true;
        }
        if !cached.description.is_empty() && m.description != cached.description {
            m.description = cached.description.clone();
            touched = true;
        }
        if !cached.genres.is_empty() && m.genres != cached.genres {
            m.genres.clone_from(&cached.genres);
            touched = true;
        }
        if !cached.year.is_empty() && m.year != cached.year {
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
        let (backdrop_to_load, poster_to_load, header, header_cache, modal_type, reveal_related) = {
            let mut state = self.shared.lock().unwrap();
            let m = match state.modal_item.as_mut() {
                Some(m) if m.id == id => m,
                _ => return,
            };
            let mut backdrop_to_load: Option<String> = None;
            let mut poster_to_load: Option<String> = None;
            let had_related = !m.details.related.is_empty();
            m.details
                .merge(&detail_extras::DetailExtras::from_item(item));
            let reveal_related = !had_related && !m.details.related.is_empty();
            // Art upgrades also apply to finished shows and deeper episode
            // views: season cards must not stay on their cached fallback.
            m.season_backdrops.extend(item.season_backdrops());
            if let Some(bg) = &item.preview.background
                && !bg.is_empty()
                && (m.background_url.is_empty() || overwrite)
            {
                m.background_url = bg.clone();
                backdrop_to_load = Some(bg.clone());
            }
            if let Some(logo) = item
                .preview
                .logo
                .as_deref()
                .filter(|url| !url.trim().is_empty())
            {
                // Logo upgrades remain independent of finished-show text policy.
                m.logo_url = logo.to_string();
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
                details: m.details.clone(),
                poster_url: read_meta_header_for(&m.type_, &m.id)
                    .map(|header| header.poster_url)
                    .unwrap_or_default(),
                background_url: m.background_url.clone(),
                logo_url: m.logo_url.clone(),
                description: m.description.clone(),
                genres: m.genres.clone(),
                year: m.year.clone(),
                season_backdrops: m.season_backdrops.clone(),
            };
            let modal_type = m.type_.clone();
            (
                backdrop_to_load,
                poster_to_load,
                header,
                header_cache,
                modal_type,
                reveal_related,
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
        self.apply_season_cards();
        self.publish_detail_extras();
        if reveal_related {
            self.reveal_detail_similar();
        }
        self.dispatch_season_thumbs();
        if let Some(app) = self.app() {
            let state = self.shared.lock().unwrap();
            if let Some(m) = state.modal_item.as_ref()
                && m.id == id
            {
                app.set_selected_description(SharedString::from(m.description.clone()));
                // A routine identical refresh must not recreate the pills.
                let shown = app.get_selected_genre_list();
                if shown.row_count() != m.genres.len()
                    || shown.iter().zip(&m.genres).any(|(a, b)| a.as_str() != b)
                {
                    app.set_selected_genre_list(
                        Rc::new(VecModel::from(
                            m.genres.iter().map(SharedString::from).collect::<Vec<_>>(),
                        ))
                        .into(),
                    );
                }
                if !m.year.is_empty() {
                    app.set_selected_year(SharedString::from(m.year.clone()));
                }
            }
        }
        self.load_current_detail_logo();
        if let Some(url) = poster_to_load {
            // The image pipeline persists the replacement only after decoding.
            // A failed fresh URL must not replace a usable saved poster.
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
    #[cfg(test)]
    pub(super) fn apply_episode_meta(
        &self,
        id: String,
        found: Option<MetaItem>,
        refresh: bool,
        open_token: Arc<()>,
    ) {
        self.apply_episode_meta_owned(id, found, refresh, open_token, None);
    }

    pub(super) fn apply_episode_meta_owned(
        &self,
        id: String,
        found: Option<MetaItem>,
        refresh: bool,
        open_token: Arc<()>,
        receipt: Option<catalog_detail::Receipt>,
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
        if !self.catalog_detail_receipt_current(receipt.as_ref()) {
            self.publish_catalog_detail_scope();
            self.apply_season_cards();
            self.refresh_episode_rows();
            return;
        }
        if receipt.is_some() {
            // The worker already persisted fresh evidence. Reproject cards
            // without making an invalid detail selection valid again.
            self.refresh_catalog_grouping_evidence();
        }
        if !self.accept_catalog_detail_meta(receipt.as_ref(), found.as_ref()) {
            if let Some(item) = found.as_ref() {
                self.tracking_metadata(id.clone(), modal_type.clone(), &item.videos);
                self.upgrade_detail_meta(&id, item, true, true);
            }
            {
                let mut state = self.shared.lock().unwrap();
                if let Some(modal) = state.modal_item.as_mut() {
                    modal.episodes_loading = false;
                    modal.pending_watch_now = None;
                    if let Some(item) = found {
                        modal.videos = item.videos;
                        modal.seasons = ordered_seasons(&modal.videos);
                    }
                }
            }
            self.publish_catalog_detail_scope();
            self.apply_season_cards();
            self.refresh_episode_rows();
            return;
        }
        self.publish_catalog_detail_scope();
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
                let scoped = self.shared.lock().unwrap().catalog_detail.active();
                let videos: Vec<Video> = item
                    .videos
                    .into_iter()
                    .filter(|v| scoped || v.season.is_some())
                    .collect();
                if !refresh && self.history_stream_selected() {
                    self.update_episode_videos(id, videos);
                    self.request_progress_projection();
                    return;
                }
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
                    self.request_progress_projection();
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
                self.request_progress_projection();
            }
            None => {
                // No addon can list episodes right now: keep showing a cached
                // list when we have one, otherwise fall back to the plain
                // streams behaviour.
                eprintln!("nova: detail refresh {modal_type}/{id}: no meta addon answered");
                let had_cache = read_episodes_cache_for(&modal_type, &id)
                    .is_some_and(|videos| !ordered_seasons(&videos).is_empty());
                if !had_cache && !self.history_stream_selected() {
                    self.start_stream_search(id);
                    self.refresh_watch_action();
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
        self.observe_catalog_detail_inventory(&id, &videos);
        let seasons = ordered_seasons(&videos);
        if seasons.is_empty() && !self.shared.lock().unwrap().catalog_detail.active() {
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
            let selected_season = m.seasons.get(m.season_index).copied();
            m.videos = videos;
            m.seasons = seasons;
            m.season_index = selected_season
                .and_then(|season| m.seasons.iter().position(|&s| s == season))
                .unwrap_or_else(|| m.season_index.min(m.seasons.len().saturating_sub(1)));
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
                apply_stream_episode_context(&app, Some(&video));
                if thumbnail_changed
                    && !app.get_player_open()
                    && let Some(url) = video.thumbnail.filter(|url| !url.is_empty())
                {
                    self.load_player_episode_backdrop(id, video.id, url);
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

    /// Remember content choices for the next visit, not navigation focus.
    /// No-op when no modal is open.
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
            let season_val = (!state.catalog_detail.active())
                .then(|| m.seasons.get(m.season_index).copied())
                .flatten();
            let filter = app.get_episode_filter().to_string();
            // A later visit restores the content choices underneath, not
            // the temporary stream selector or its navigation position.
            (
                m.id.clone(),
                DetailSnapshot {
                    // Tracking drafts belong to the active worker context;
                    // reopening restores the previous content tab instead.
                    tab: if app.get_detail_tab() == 4 {
                        app.get_tracking_return_tab()
                    } else {
                        app.get_detail_tab()
                    },
                    season: season_val,
                    filter,
                },
            )
        };
        self.shared
            .lock()
            .unwrap()
            .detail_snapshots
            .insert(snap.0, snap.1);
    }

    /// Choose the initial tab/filter before mounting; season resolves when
    /// metadata arrives. Keyboard navigation starts at Back on every visit.
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
        let tab = if app.get_detail_is_movie() {
            0
        } else {
            snap.as_ref().map_or(3, |s| if s.tab == 0 { 0 } else { 3 })
        };
        app.set_detail_tab(tab);
        app.set_episode_filter(snap.map_or_else(SharedString::default, |s| s.filter.into()));
        app.set_detail_kb_zone(0);
        app.set_detail_kb_top(0);
        app.set_detail_kb_tab(tab);
        app.set_detail_kb_ci(2);
        app.set_detail_kb_ep(0);
        app.set_detail_kb_s(0);
    }

    /// Main thread: switch the modal to the season/episode picker.
    /// Build the episode-row model for the modal item's currently selected
    /// season, filling decoded thumbnails from the process-wide poster cache.
    /// Parse an ISO 8601 date string and return a human-friendly date.
    /// When `relative` is true, returns relative strings ("3 days ago");
    /// when false, always returns an absolute date ("Mar 15, 2024").
    pub(super) fn format_human_date(iso: &str, relative: bool) -> Option<String> {
        let (year, month, day) = iso_date(iso)?;

        let abs = || text::absolute_date(day as u32, month as u32, year);

        if !relative {
            return Some(abs());
        }

        let days = iso_days(iso)?;

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

    /// Bound the episode overview and resume suffix together before the UI
    /// wraps and elides them inside the card's two-line synopsis viewport.
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
            Some(p) if p.watched => (
                true,
                // A manual watched mark has no playback position. Keep its
                // checkmark without inventing a completed playback rail.
                if p.position_secs > 0.0 { 1.0 } else { 0.0 },
                String::new(),
            ),
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

    pub(super) fn current_episode_page(&self) -> (usize, usize, Vec<EpisodeRow>) {
        struct RowSeed {
            id: SharedString,
            artwork_identity: SharedString,
            text: SharedString,
            details: String,
            thumb_url: Option<String>,
            show_thumb: bool,
            watched: bool,
            progress: f32,
            ep_no: SharedString,
            date: SharedString,
            runtime: SharedString,
        }
        let (total, page, seeds) = {
            let state = self.shared.lock().unwrap();
            let Some(m) = state.modal_item.as_ref() else {
                return (0, 0, Vec::new());
            };
            let visible_seasons = catalog_detail::seasons(&state, m);
            if visible_seasons.is_empty() {
                return (0, 0, Vec::new());
            }
            let date_relative = state.cache_settings.date_relative;
            let show_unwatched = state.cache_settings.show_unwatched_thumbs;
            let season = visible_seasons[m.season_index.min(visible_seasons.len() - 1)];
            // The card filter box narrows the same season list the picker
            // shows (episode_picked resolves through this same filter).
            let filter = self
                .app()
                .map(|a| a.get_episode_filter().to_string())
                .unwrap_or_default();
            let episodes: Vec<_> = catalog_detail::episodes(&state, m, season)
                .into_iter()
                .filter(|v| episode_matches_filter(v, &filter))
                .collect();
            let total = episodes.len();
            let page = m.episode_page.min(page_count(total).saturating_sub(1));
            // Do not construct images or display strings for other pages.
            let seeds: Vec<RowSeed> = episodes
                .into_iter()
                .skip(page * EPISODE_PAGE_SIZE)
                .take(EPISODE_PAGE_SIZE)
                .map(|v| {
                    let (watched, progress, resume_suffix) =
                        Self::episode_watch_state(&state.progress, &m.id, &v.id);
                    let label = episode_row_label(v);
                    // No checkmark prefix: watched state already shows on the
                    // artwork disc overlay.
                    let text = SharedString::from(label);
                    // The artwork card keeps release metadata separate from
                    // its two-line synopsis. Bound the supplied overview before
                    // Slint elides it at the current card width.
                    let details = Self::episode_details(v.overview.as_deref(), &resume_suffix);
                    let date = v
                        .released
                        .as_deref()
                        .and_then(|d| {
                            Self::format_human_date(d, date_relative)
                                .or_else(|| Some(d.trim().to_string()))
                        })
                        .unwrap_or_default();
                    let show_thumb = Self::episode_thumb_visible(show_unwatched, watched, progress);
                    RowSeed {
                        id: v.id.as_str().into(),
                        artwork_identity: detail_artwork_identity(
                            m,
                            "episode",
                            &v.id,
                            show_thumb.then_some(v.thumbnail.as_deref()).flatten(),
                        )
                        .into(),
                        text,
                        details,
                        thumb_url: v.thumbnail.clone(),
                        show_thumb,
                        watched,
                        progress,
                        ep_no: SharedString::from(episode_badge(v)),
                        date: SharedString::from(date),
                        // The SDK's optional Video runtime is preserved in
                        // Video.extra; never substitute a series-wide average.
                        runtime: SharedString::from(
                            v.extra
                                .get("runtime")
                                .and_then(serde_json::Value::as_str)
                                .map(str::trim)
                                .unwrap_or_default(),
                        ),
                    }
                })
                .collect();
            (total, page, seeds)
        };

        let rows = seeds
            .into_iter()
            .map(|seed| {
                let reserved_lines = Self::estimate_stream_lines(&seed.text)
                    + if seed.details.is_empty() {
                        0
                    } else {
                        Self::estimate_stream_lines(&seed.details)
                    };
                let lines = reserved_lines.max(1);
                // Episode rows use the configured thumbnail recipe, matching
                // the pump's cache key even when Original is selected.
                // Hidden by the `show_unwatched_thumbs` setting for fresh
                // episodes (placeholder instead).
                let (thumb, has_thumb) = if !seed.show_thumb {
                    (Image::default(), false)
                } else {
                    match &seed.thumb_url {
                        Some(url) => {
                            match decoded_cache_get(&image_cache_key(
                                url,
                                ImageUse::EpisodeThumbnail,
                            )) {
                                Some(buf) => (Image::from_rgba8(buf), true),
                                None => (Image::default(), false),
                            }
                        }
                        None => (Image::default(), false),
                    }
                };
                EpisodeRow {
                    id: seed.id,
                    displayed_artwork_identity: if has_thumb {
                        seed.artwork_identity.clone()
                    } else {
                        SharedString::default()
                    },
                    artwork_identity: seed.artwork_identity,
                    text: seed.text,
                    details: SharedString::from(&seed.details),
                    lines,
                    thumb,
                    has_thumb,
                    watched: seed.watched,
                    progress: seed.progress,
                    ep_no: seed.ep_no,
                    date: seed.date,
                    runtime: seed.runtime,
                }
            })
            .collect();
        (total, page, rows)
    }

    /// Render the current filtered page, preserving correctly identified live
    /// images even when their pixels have been evicted from the shared cache.
    pub(super) fn apply_episode_rows(&self) {
        self.refresh_catalog_detail_season_names();
        let Some(app) = self.app() else {
            return;
        };
        self.refresh_watch_action();
        self.refresh_home_watch_action();
        let (total, page, mut rows) = self.current_episode_page();
        if let Some(m) = self.shared.lock().unwrap().modal_item.as_mut() {
            m.episode_page = page;
        }
        let old = app.get_episode_rows();
        for row in &mut rows {
            if let Some(previous) = old.iter().find(|previous| previous.id == row.id) {
                retain_episode_row_artwork(row, &previous);
            }
        }
        let start = page * EPISODE_PAGE_SIZE;
        app.set_episode_rows(Rc::new(VecModel::from(rows)).into());
        app.set_episode_page(page as i32);
        app.set_episode_page_count(page_count(total) as i32);
        app.set_episode_page_start(start as i32);
        app.set_episode_total(total as i32);
        // Late detail enrichment can supply the mappings needed by an open
        // move review. Recompute its counts instead of leaving an initial 0.
        if app.get_library_duplicates_open() && app.get_library_duplicate_selection() >= 0 {
            self.library_duplicate_action(app.get_library_duplicate_selection());
        }
    }

    /// Repaint a manual progress edit without rebuilding delegates, reading
    /// offscreen metadata or queuing artwork. Visible rows are bounded by the
    /// episode pager; season fractions need only one pass through modal videos.
    pub(super) fn refresh_detail_progress_ui(&self) {
        let Some(app) = self.app() else { return };
        let episodes = app.get_episode_rows();
        let seasons = app.get_season_cards();
        let (episode_updates, season_updates, completed) = {
            let state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_ref() else {
                return;
            };
            let videos: HashMap<_, _> = modal
                .videos
                .iter()
                .map(|video| (video.id.as_str(), video))
                .collect();
            let mut episode_updates = Vec::new();
            for (index, mut row) in episodes.iter().enumerate() {
                let Some(video) = videos.get(row.id.as_str()) else {
                    continue;
                };
                let old = row.clone();
                let (watched, progress, suffix) =
                    Self::episode_watch_state(&state.progress, &modal.id, &video.id);
                row.watched = watched;
                row.progress = progress;
                row.details = Self::episode_details(video.overview.as_deref(), &suffix).into();
                row.lines = (Self::estimate_stream_lines(&row.text)
                    + if row.details.is_empty() {
                        0
                    } else {
                        Self::estimate_stream_lines(&row.details)
                    })
                .max(1);
                let visible = Self::episode_thumb_visible(
                    state.cache_settings.show_unwatched_thumbs,
                    watched,
                    progress,
                );
                row.artwork_identity = detail_artwork_identity(
                    modal,
                    "episode",
                    &video.id,
                    visible.then_some(video.thumbnail.as_deref()).flatten(),
                )
                .into();
                if !visible || row.artwork_identity.is_empty() {
                    row.thumb = Image::default();
                    row.has_thumb = false;
                    row.displayed_artwork_identity = SharedString::default();
                } else if !row.has_thumb || row.displayed_artwork_identity != row.artwork_identity {
                    // Only the decoded memory LRU is consulted. A newly visible
                    // uncached thumbnail remains the existing placeholder.
                    let pixels = video.thumbnail.as_deref().and_then(|url| {
                        decoded_cache_get(&image_cache_key(url, ImageUse::EpisodeThumbnail))
                    });
                    row.has_thumb = pixels.is_some();
                    row.thumb = pixels.map(Image::from_rgba8).unwrap_or_default();
                    row.displayed_artwork_identity = if row.has_thumb {
                        row.artwork_identity.clone()
                    } else {
                        SharedString::default()
                    };
                }
                if old.watched != row.watched
                    || old.progress != row.progress
                    || old.details != row.details
                    || old.lines != row.lines
                    || old.artwork_identity != row.artwork_identity
                    || old.displayed_artwork_identity != row.displayed_artwork_identity
                    || old.has_thumb != row.has_thumb
                    || old.thumb != row.thumb
                {
                    episode_updates.push((index, row));
                }
            }
            let mut fractions: HashMap<u32, (usize, usize)> = HashMap::new();
            let visible_videos = catalog_detail::visible_native_videos(&state, modal);
            let virtual_season = state
                .catalog_detail
                .active()
                .then(|| catalog_detail::seasons(&state, modal).first().copied())
                .flatten();
            for video in visible_videos.iter() {
                let Some(season) = virtual_season.or(video.season) else {
                    continue;
                };
                let tally = fractions.entry(season).or_default();
                tally.0 += 1;
                tally.1 += usize::from(
                    state
                        .progress
                        .get(&progress_map_key(&modal.id, &video.id))
                        .is_some_and(|progress| progress.watched),
                );
            }
            let mut season_updates = Vec::new();
            for (index, mut row) in seasons.iter().enumerate() {
                let (total, watched) = row
                    .id
                    .parse::<u32>()
                    .ok()
                    .and_then(|season| fractions.get(&season).copied())
                    .unwrap_or_default();
                let complete = total > 0 && watched == total;
                let fraction = if total > 0 {
                    watched as f32 / total as f32
                } else {
                    0.0
                };
                if row.watched != complete || row.progress != fraction {
                    row.watched = complete;
                    row.progress = fraction;
                    season_updates.push((index, row));
                }
            }
            let completed = library_title_completed(
                &modal.type_,
                &modal.id,
                &modal.videos,
                &state.progress,
                today_days(),
            );
            (episode_updates, season_updates, completed)
        };
        // Model notifications may evaluate bindings; release Shared first.
        for (index, row) in episode_updates {
            episodes.set_row_data(index, row);
        }
        for (index, row) in season_updates {
            seasons.set_row_data(index, row);
        }
        app.set_detail_library_watched(completed);
        // Activity metadata is prepared off-thread. Hide the stale Wait/Resume
        // action until the current projection publishes its replacement.
        app.set_detail_library_activity_action(0);
        self.refresh_watch_action_label();
    }

    /// Main thread: the Episodes tab pager moved to `page` (absolute; clamped
    /// here). Rebuilds the page's rows and queues its thumbnails.
    pub(super) fn episode_page_picked(&self, page: i32) {
        self.cancel_watch_now();
        let total = {
            let state = self.shared.lock().unwrap();
            state.modal_item.as_ref().map_or(0, |m| {
                let filter = self
                    .app()
                    .map(|a| a.get_episode_filter().to_string())
                    .unwrap_or_default();
                catalog_detail::seasons(&state, m)
                    .get(m.season_index)
                    .map_or(0, |&season| {
                        catalog_detail::episodes(&state, m, season)
                            .into_iter()
                            .filter(|v| episode_matches_filter(v, &filter))
                            .count()
                    })
            })
        };
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
            self.refresh_episode_rows();
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
    /// thumbnail downloads. Big seasons construct only this page's rows, and
    /// episode/season requests share the capped download pool.
    pub(super) fn refresh_episode_rows(&self) {
        // Cancel queued downloads that belonged to a previous list.
        let cancelled: Vec<_> = EPISODE_QUEUE.lock().unwrap().drain(..).collect();
        for job in cancelled {
            Self::release_episode_thumb_job(&job);
        }
        self.apply_episode_rows();
        self.queue_episode_thumbnails();
        // Rebuilding an episode page also cancels queued season-card jobs.
        // Requeue missing picker images after the visible episode work.
        self.dispatch_season_thumbs();
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
        catalog_detail::seasons(&state, m)
            .iter()
            .map(|&s| {
                let url = catalog_detail::season_artwork(&state, m, s);
                let artwork_identity =
                    detail_artwork_identity(m, "season", &s.to_string(), url.as_deref());
                let (thumb, has_thumb) = match url.as_deref().and_then(|url| {
                    decoded_cache_get(&image_cache_key(url, ImageUse::EpisodeThumbnail))
                }) {
                    Some(buf) => (Image::from_rgba8(buf), true),
                    None => (Image::default(), false),
                };
                // Watched fraction from the progress map (manual toggles and
                // playback both land here). Every known episode counts,
                // including unaired ones: a season with episodes still to
                // come never reads fully watched.
                let eps = catalog_detail::episodes(&state, m, s);
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
                    id: s.to_string().into(),
                    displayed_artwork_identity: if has_thumb {
                        artwork_identity.as_str().into()
                    } else {
                        SharedString::default()
                    },
                    artwork_identity: artwork_identity.into(),
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
            let mut rows = self.current_season_cards();
            let old = app.get_season_cards();
            for row in &mut rows {
                if let Some(previous) = old.iter().find(|previous| previous.id == row.id) {
                    retain_season_artwork(row, &previous);
                }
            }
            app.set_season_cards(Rc::new(VecModel::from(rows)).into());
        }
    }

    pub(super) fn show_episode_picker(&self, id: String, videos: Vec<Video>) {
        self.observe_catalog_detail_inventory(&id, &videos);
        let seasons = ordered_seasons(&videos);
        if seasons.is_empty() && !self.shared.lock().unwrap().catalog_detail.active() {
            self.refresh_watch_action();
            self.resolve_watch_now();
            self.start_stream_search(id);
            return;
        }
        let (first_population, previous_season) = {
            let mut state = self.shared.lock().unwrap();
            let m = match state.modal_item.as_mut() {
                Some(m) if m.id == id => m,
                _ => return,
            };
            let first_population = m.seasons.is_empty();
            let previous_season = m.seasons.get(m.season_index).copied();
            m.videos = videos;
            m.seasons = seasons;
            if first_population {
                state.detail_opening_season = Some(m.open_token.clone());
            }
            (first_population, previous_season)
        };
        // History aliases are reconciled when metadata/history changes, by
        // the guarded progress worker. A cached opening only reads this title.
        let (season_names, season_idx) = {
            let mut state = self.shared.lock().unwrap();
            let m = state.modal_item.as_ref().unwrap();
            let season = if first_population && !state.catalog_detail.active() {
                last_watched_episode(&state, m)
                    .and_then(|video| video.season)
                    .or_else(|| state.detail_snapshots.get(&id).and_then(|s| s.season))
            } else {
                previous_season
            };
            let scoped = state.catalog_detail.active();
            let m = state.modal_item.as_mut().unwrap();
            m.season_index = season
                .filter(|_| !scoped)
                .and_then(|season| m.seasons.iter().position(|&s| s == season))
                .unwrap_or(0);
            if first_population {
                // Opening selects only the season; playback-return handles
                // selecting a page/episode and requesting a scroll separately.
                m.episode_page = 0;
            }
            let names = m
                .seasons
                .iter()
                .map(|&s| SharedString::from(season_label(s)))
                .collect::<Vec<_>>();
            (names, m.season_index)
        };
        if let Some(app) = self.app() {
            app.set_modal_episodes(true);
            app.set_detail_deep_stream(false);
            apply_stream_episode_context(&app, None);
            app.set_season_names(Rc::new(VecModel::from(season_names)).into());
            app.set_season_combo_idx(season_idx as i32);
            self.clear_streams();
            self.set_stream_hint(None);
            // The tab was chosen before mounting. A late response must not
            // overwrite an Overview choice made while episodes were loading.
        }
        self.apply_season_cards();
        self.dispatch_season_thumbs();
        self.refresh_episode_rows();
        // Cached metadata can satisfy Watch Now before the refresh answers.
        self.resolve_watch_now();
    }

    /// Reapply an opening's history anchor only while navigation is untouched.
    pub(super) fn restore_opening_season_after_projection(&self) {
        let index = {
            let mut state = self.shared.lock().unwrap();
            let Some(token) = state.detail_opening_season.take() else {
                return;
            };
            let Some(modal) = state.modal_item.as_ref() else {
                return;
            };
            if !Arc::ptr_eq(&token, &modal.open_token) || state.catalog_detail.active() {
                return;
            }
            let season = last_watched_episode(&state, modal).and_then(|video| video.season);
            let index = season.and_then(|season| modal.seasons.iter().position(|s| *s == season));
            let Some(index) = index.filter(|index| *index != modal.season_index) else {
                return;
            };
            let modal = state.modal_item.as_mut().unwrap();
            modal.season_index = index;
            modal.episode_page = 0;
            index
        };
        if let Some(app) = self.app() {
            app.set_season_combo_idx(index as i32);
            app.set_detail_kb_ep(0);
        }
        self.refresh_episode_rows();
    }

    /// Main thread: switch the episode list to another season.
    pub(super) fn season_picked(&self, index: usize) {
        self.cancel_season_download();
        self.cancel_watch_now();
        self.reset_episode_page();
        let idx = {
            let mut state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_ref() else {
                return;
            };
            let total = catalog_detail::seasons(&state, modal).len();
            if total == 0 {
                return;
            }
            let m = state.modal_item.as_mut().unwrap();
            m.season_index = index.min(total - 1);
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
    /// Uses the same season + filter as `current_episode_page`, so menu
    /// actions and the quick-toggle always hit the card the user touched.
    pub(super) fn resolve_filtered_episode(
        &self,
        index: usize,
    ) -> Option<(String, String, Vec<String>)> {
        let state = self.shared.lock().unwrap();
        let m = state.modal_item.as_ref()?;
        let visible_seasons = catalog_detail::seasons(&state, m);
        let season =
            *visible_seasons.get(m.season_index.min(visible_seasons.len().saturating_sub(1)))?;
        let filter = self
            .app()
            .map(|a| a.get_episode_filter().to_string())
            .unwrap_or_default();
        let season_eps = catalog_detail::episodes(&state, m, season);
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
            // A second explicit Watch remains an action even if another alias
            // was just unwatched and this row's projection is still true.
            entry.manual_watch_action = Some(next_manual_watch_action());
            if let Some(history) = entry.history.as_mut() {
                history.rewatch = false;
            }
            // A fresh watch intent supersedes any earlier unwatch, so the
            // mesh merge can't mistake the stale unwatch for current intent.
            entry.unwatched_at_secs = 0;
            entry.updated_at_secs = now;
        } else {
            // The other provider's Watch can still be awaiting projection.
            // Explicit Unwatch must establish its own register even if this
            // provider has no materialized row yet.
            let entry = map.entry(key).or_insert_with(|| EpisodeProgress {
                series_id: series_id.to_string(),
                episode_id: episode_id.to_string(),
                ..Default::default()
            });
            entry.watched = false;
            entry.manual_watch_action = Some(next_manual_watch_action());
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
            state.detail_opening_season = None;
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
            let visible_seasons = catalog_detail::seasons(&state, m);
            let Some(&season) = visible_seasons.get(index) else {
                return;
            };
            // Bulk actions skip unaired episodes (known future air date).
            let eps: Vec<&Video> = catalog_detail::episodes(&state, m, season)
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
        self.cancel_season_download();
        self.cancel_watch_now();
        let (series_id, request_id, video) = {
            let state = self.shared.lock().unwrap();
            let m = match state.modal_item.as_ref() {
                Some(m) => m,
                _ => return,
            };
            let visible_seasons = catalog_detail::seasons(&state, m);
            let Some(&season) = visible_seasons.get(m.season_index) else {
                return;
            };
            // Same filter current_episode_page applies, so a filtered card
            // opens its own episode (row order matches the list).
            let filter = self
                .app()
                .map(|a| a.get_episode_filter().to_string())
                .unwrap_or_default();
            let season_eps = catalog_detail::episodes(&state, m, season);
            let episodes: Vec<&&Video> = season_eps
                .iter()
                .filter(|v| episode_matches_filter(v, &filter))
                .collect();
            let video = match episodes.get(index) {
                Some(v) => v,
                None => return,
            };
            (m.id.clone(), video.id.clone(), (***video).clone())
        };
        self.select_episode_streams(series_id, request_id, video, index);
    }

    /// Select by stable ID, independent of the current filter/season/page.
    pub(super) fn episode_streams_by_id(&self, series_id: &str, episode_id: &str) {
        self.cancel_watch_now();
        let selection = {
            let mut state = self.shared.lock().unwrap();
            let Some(m) = state.modal_item.as_ref().filter(|m| m.id == series_id) else {
                return;
            };
            if !catalog_detail::allows(&state, m, episode_id) {
                return;
            }
            let Some(video) = m.videos.iter().find(|v| v.id == episode_id).cloned() else {
                return;
            };
            let visible = catalog_detail::seasons(&state, m);
            let season_index = if state.catalog_detail.active() {
                0
            } else {
                let Some(index) = visible.iter().position(|s| Some(*s) == video.season) else {
                    return;
                };
                index
            };
            let Some(&season) = visible.get(season_index) else {
                return;
            };
            let Some(index) = catalog_detail::episodes(&state, m, season)
                .iter()
                .position(|v| v.id == episode_id)
            else {
                return;
            };
            let m = state.modal_item.as_mut().unwrap();
            m.season_index = season_index;
            m.episode_page = index / EPISODE_PAGE_SIZE;
            (video, season_index, index)
        };
        if let Some(app) = self.app() {
            app.set_episode_filter(SharedString::default());
            app.set_season_combo_idx(selection.1 as i32);
        }
        self.refresh_episode_rows();
        self.apply_season_cards();
        self.select_episode_streams(
            series_id.to_owned(),
            episode_id.to_owned(),
            selection.0,
            selection.2,
        );
    }

    pub(super) fn select_episode_streams(
        &self,
        series_id: String,
        request_id: String,
        video: Video,
        index: usize,
    ) {
        self.persist_outgoing_playback();
        // Arm playback tracking for this episode, resolving a resume point
        // from history (fresh when watched / no saved position).
        {
            let mut state = self.shared.lock().unwrap();
            let resume_pos = state
                .progress
                .get(&progress_map_key(&series_id, &request_id))
                .filter(|p| playback::playback_can_resume(p))
                .map(|p| p.position_secs);
            state.playback = Some(PlaybackTarget {
                series_id,
                episode_id: request_id.clone(),
                resume_pos,
                ..Default::default()
            });
        }
        let mut preview_work = None;
        if let Some(app) = self.app() {
            // A previous playback reveal must not survive a new manual pick:
            // cancelling this selector returns to the retained list offset.
            app.set_detail_episode_reveal_request(0);
            app.set_modal_episodes(false);
            app.set_stream_selector_open(true);
            // A manual pick means the episode list was visited, so any
            // deep-link back shortcut ends here (`continue_picked` sets it
            // again afterwards for its own auto-opened streams).
            app.set_detail_deep_stream(false);
            apply_stream_episode_context(&app, Some(&video));
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
            // Prefer a ready episode thumbnail without first resizing the
            // poster it replaces. Large cached images are resized on a worker,
            // so opening the selector never waits for a Triangle resample.
            let pixels = app
                .get_episode_rows()
                .row_data(local_index)
                .filter(|row| row.has_thumb)
                .and_then(|row| row.thumb.to_rgba8())
                .or_else(|| app.get_selected_poster().to_rgba8());
            app.set_player_poster(Image::default());
            if let Some(pixels) = pixels {
                let settings = active_cache_settings();
                let maximum =
                    ImageRecipe::resolve(ImageUse::PlayerEpisodeBackdrop, &settings).max_side;
                if maximum
                    .is_some_and(|side| side > 0 && pixels.width().max(pixels.height()) > side)
                {
                    preview_work = Some((pixels, settings, image_settings_revision()));
                } else {
                    app.set_player_poster(Image::from_rgba8(pixels));
                }
            }
        }
        // Use the cached thumbnail when ready, then upgrade through the player's recipe.
        if let Some(url) = video.thumbnail.clone() {
            let series_id = self
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .map(|item| item.id.clone());
            if let Some(series_id) = series_id {
                self.load_player_episode_backdrop(series_id, video.id, url);
            }
        }
        self.start_stream_search(request_id);
        if let Some((pixels, settings, revision)) = preview_work {
            self.resize_player_episode_preview(pixels, settings, revision);
        }
    }

    fn resize_player_episode_preview(
        &self,
        pixels: SharedPixelBuffer<Rgba8Pixel>,
        settings: CacheSettings,
        image_revision: u64,
    ) {
        let request = {
            let state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_ref() else {
                return;
            };
            EpisodePreviewRequest {
                open_token: modal.open_token.clone(),
                episode_id: modal.request_id.clone(),
                stream_generation: state.stream_generation,
                image_revision,
            }
        };
        let bridge = self.clone();
        // Both the resize and completion carry the originating profile.
        thread::Builder::new()
            .name("episode-preview".into())
            .spawn(move || {
                let pixels = player_episode_preview(&pixels, &settings);
                let _ = profiles::invoke_from_event_loop(move || {
                    let Some(app) = bridge.app() else { return };
                    let state = bridge.shared.lock().unwrap();
                    let current = request.accepts(
                        &state,
                        image_settings_revision(),
                        app.get_player_poster().size().width > 0,
                    );
                    drop(state);
                    if current {
                        app.set_player_poster(Image::from_rgba8(pixels));
                    }
                });
            })
            .expect("spawn episode preview resize");
    }

    fn load_player_episode_backdrop(&self, series_id: String, episode_id: String, url: String) {
        let token = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .filter(|item| item.id == series_id)
            .map(|item| item.open_token.clone());
        let Some(token) = token else { return };
        let bridge = self.clone();
        let revision = image_settings_revision();
        let requested_url = url.clone();
        net::fetch_image(url, ImageUse::PlayerEpisodeBackdrop, move |pixels| {
            let Some(pixels) = pixels else { return };
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                let state = bridge.shared.lock().unwrap();
                let current = state.modal_item.as_ref().is_some_and(|item| {
                    Arc::ptr_eq(&item.open_token, &token)
                        && item.id == series_id
                        && item.request_id == episode_id
                        && item.videos.iter().any(|video| {
                            video.id == episode_id
                                && video.thumbnail.as_deref() == Some(&requested_url)
                        })
                });
                drop(state);
                if current
                    && image_settings_revision() == revision
                    && let Some(app) = bridge.app()
                {
                    app.set_player_poster(Image::from_rgba8(pixels));
                }
            });
        });
    }

    /// Main thread: go back from an episode's streams to the episode list.
    pub(super) fn episodes_back(&self) {
        self.player.release_window_transition();
        self.cancel_season_download();
        self.cancel_watch_now();
        let is_episodic = {
            let state = self.shared.lock().unwrap();
            state
                .modal_item
                .as_ref()
                .map(|m| !catalog_detail::seasons(&state, m).is_empty())
                .unwrap_or(false)
        };
        if !is_episodic {
            return;
        }
        if let Some(app) = self.app() {
            app.set_modal_episodes(true);
            app.set_stream_selector_open(false);
            // The episode list is on screen now, so a deep-link back shortcut
            // no longer applies (system back returns here from new streams).
            app.set_detail_deep_stream(false);
            apply_stream_episode_context(&app, None);
            self.clear_streams();
            self.set_stream_hint(None);
            // Reveal the Episodes tab underneath the fullscreen selector.
            app.set_detail_tab(3);
            app.set_detail_kb_zone(3);
            app.set_detail_kb_ci(2);
        }
    }

    /// Restore the last observed episode, not the next unwatched candidate.
    /// Clear a stale filter so a recently played episode cannot remain hidden.
    fn reveal_last_watched_episode(&self) {
        let Some(app) = self.app() else { return };
        let selection = {
            let mut state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_ref() else {
                return;
            };
            let video = last_watched_episode(&state, modal).cloned();
            video.and_then(|video| {
                let modal = state.modal_item.as_ref()?;
                let visible = catalog_detail::seasons(&state, modal);
                let season_index = if state.catalog_detail.active() {
                    0
                } else {
                    visible
                        .iter()
                        .position(|season| Some(*season) == video.season)?
                };
                let index = catalog_detail::episodes(&state, modal, *visible.get(season_index)?)
                    .iter()
                    .position(|v| v.id == video.id)?;
                let modal = state.modal_item.as_mut()?;
                modal.season_index = season_index;
                modal.episode_page = index / EPISODE_PAGE_SIZE;
                Some((season_index, index % EPISODE_PAGE_SIZE))
            })
        };
        if let Some((season_index, local_index)) = selection {
            app.set_episode_filter(SharedString::default());
            app.set_season_combo_idx(season_index as i32);
            self.refresh_episode_rows();
            self.apply_season_cards();
            app.set_detail_kb_ep(local_index as i32);
        }
        app.set_detail_episode_reveal_request(app.get_detail_episode_reveal_request() + 1);
    }

    /// A player opened from Detail returns to the watched episode in its list.
    /// Home shortcuts retain their own return route.
    pub(super) fn restore_detail_after_playback(&self) {
        if let Some(app) = self.app()
            && app.get_modal_visible()
            && !app.get_detail_deep_stream()
            && !app.get_episode_context().is_empty()
        {
            self.episodes_back();
            self.reveal_last_watched_episode();
        }
    }

    /// Detail tab bar: 0 = Overview, 3 = Episodes (1/2 are addon-less stubs).
    /// The episode list remains on this tab underneath its stream selector.
    pub(super) fn detail_tab_picked(&self, tab: i32) {
        self.cancel_season_download();
        self.cancel_watch_now();
        let Some(app) = self.app() else {
            return;
        };
        if tab == 4 && app.get_in_library() {
            self.tracking_show();
            return;
        }
        if app.get_tracking_open() {
            self.tracking_close();
        }
        let tab = if tab == 3 && !app.get_detail_is_movie() {
            3
        } else {
            0
        };
        app.set_detail_tab(tab);
        app.set_detail_kb_tab(if tab == 3 { 3 } else { 0 });
        if tab == 0 {
            self.load_detail_similar();
        }
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

    /// Keep the advertised episode identical to the primary action's target,
    /// independent of the selected season, text filter or episode page.
    pub(super) fn refresh_watch_action(&self) {
        self.publish_catalog_detail_scope();
        // Opening a title only needs its own flags, not offscreen queue rows
        // or the complete Calendar subscription/schedule projection.
        self.refresh_detail_queue_state();
        self.refresh_detail_reminder_state();
        self.refresh_watch_action_label();
    }

    // Manual episode edits only need this modal-local label. Rebuilding the
    // offscreen queue/Home surface belongs to the background projection path.
    fn refresh_watch_action_label(&self) {
        let label = {
            let state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_ref() else {
                return;
            };
            if !catalog_detail::ready(&state, modal) {
                text::tr("Choose an episode").into()
            } else if modal.type_ == "movie"
                || (!state.catalog_detail.active()
                    && !modal.episodes_loading
                    && modal.seasons.is_empty())
            {
                watch_action_label(&modal.id, None, &state.progress)
            } else {
                let videos = catalog_detail::view_videos(&state, modal);
                watch_action_label(&modal.id, Some(&videos), &state.progress)
            }
        };
        if let Some(app) = self.app() {
            app.set_detail_watch_label(label.into());
        }
    }

    /// Movie and episode actions open the same selector; streams remain a
    /// manual choice so quality/addon preferences are respected.
    pub(super) fn watch_now(&self) {
        self.publish_catalog_detail_scope();
        {
            let state = self.shared.lock().unwrap();
            if state
                .modal_item
                .as_ref()
                .is_some_and(|m| !catalog_detail::ready(&state, m))
            {
                return;
            }
        }
        let direct_streams = {
            let mut state = self.shared.lock().unwrap();
            state.detail_opening_season = None;
            let scoped = state.catalog_detail.active();
            let Some(modal) = state.modal_item.as_mut() else {
                return;
            };
            // Some addons can stream a title without providing episodes.
            // Keep that existing fallback reachable through the hero action.
            let direct = modal.type_ == "movie"
                || (!scoped && !modal.episodes_loading && modal.seasons.is_empty());
            modal.pending_watch_now = if direct {
                None
            } else {
                Some(WatchNowOrigin::Detail)
            };
            direct
        };
        let Some(app) = self.app() else {
            return;
        };
        if direct_streams {
            app.set_detail_deep_stream(false);
            app.set_detail_kb_s(0);
            app.set_stream_selector_open(true);
            return;
        }
        app.set_detail_tab(3);
        app.set_detail_kb_zone(3);
        self.resolve_watch_now();
    }

    /// Back preserves the origin of selection: manual episodes return to
    /// their list, movies to Overview, and Home shortcuts to the root page.
    pub(super) fn stream_selector_back(&self) {
        self.player.release_window_transition();
        if self.season_download_is_replacement() {
            self.cancel_season_download();
            if let Some(app) = self.app() {
                app.set_modal_visible(false);
            }
            self.modal_closed();
            return;
        }
        self.remote_disconnect();
        self.cancel_pending_for_navigation();
        let Some(app) = self.app() else {
            return;
        };
        app.invoke_preserve_back_focus();
        if app.get_detail_deep_stream() {
            app.set_modal_visible(false);
            self.modal_closed();
        } else if !app.get_episode_context().is_empty() {
            self.episodes_back();
        } else {
            app.set_stream_selector_open(false);
            app.set_stream_action_open(false);
            app.set_detail_tab(0);
            app.set_detail_kb_zone(2);
            self.load_detail_similar();
        }
    }

    fn cancel_watch_now(&self) {
        let mut state = self.shared.lock().unwrap();
        state.detail_opening_season = None;
        if let Some(modal) = state.modal_item.as_mut() {
            modal.pending_watch_now = None;
        }
    }

    /// Consume one pending intent once an eligible episode is known. With
    /// no cached candidate, wait for metadata before using the direct-stream
    /// fallback for providers that cannot list episodes.
    fn resolve_watch_now(&self) {
        {
            let state = self.shared.lock().unwrap();
            if state
                .modal_item
                .as_ref()
                .is_some_and(|m| !catalog_detail::ready(&state, m))
            {
                return;
            }
        }
        let direct_origin = {
            let mut state = self.shared.lock().unwrap();
            let scoped = state.catalog_detail.active();
            let Some(modal) = state.modal_item.as_mut() else {
                return;
            };
            if !scoped && !modal.episodes_loading && modal.seasons.is_empty() {
                modal.pending_watch_now.take()
            } else {
                None
            }
        };
        if let Some(origin) = direct_origin {
            if let Some(app) = self.app() {
                app.set_detail_kb_s(0);
                app.set_stream_selector_open(true);
                app.set_detail_deep_stream(matches!(origin, WatchNowOrigin::Featured));
            }
            return;
        }
        let selected = {
            let mut state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_ref() else {
                return;
            };
            let Some(origin) = modal.pending_watch_now else {
                return;
            };
            let visible_videos = catalog_detail::view_videos(&state, modal);
            let target =
                watch_now_episode(&modal.id, &visible_videos, &state.progress).and_then(|video| {
                    let visible = catalog_detail::seasons(&state, modal);
                    let season_index = if state.catalog_detail.active() {
                        0
                    } else {
                        visible.iter().position(|&s| Some(s) == video.season)?
                    };
                    let row = catalog_detail::episodes(&state, modal, *visible.get(season_index)?)
                        .iter()
                        .position(|v| v.id == video.id)?;
                    Some((season_index, row))
                });
            drop(visible_videos);
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
    fn stream_addon_updated(
        &self,
        context: StreamResponseContext,
        new_rows: Vec<StreamUi>,
        complete: bool,
    ) {
        let StreamResponseContext {
            request_id,
            generation,
            addon,
            owner_url,
            cache_hit,
            ..
        } = context;
        let searching = {
            let mut state = self.shared.lock().unwrap();
            if !stream_response_is_current(&state, &request_id, generation) {
                return;
            }
            refresh_stream_owner_order(&mut state);
            let selected_label = state
                .stream_filter
                .checked_sub(1)
                .and_then(|i| state.stream_addons.get(i))
                .cloned();
            if complete {
                state.stream_pending.retain(|owner| owner != &owner_url);
                if cache_hit && !new_rows.is_empty() {
                    state.stream_cached_owners.insert(owner_url.clone());
                } else {
                    state.stream_cached_owners.remove(&owner_url);
                }
            }
            // A snapshot can insert anywhere, reorder, or remove rows without
            // duplicating earlier publications or touching a namesake addon.
            replace_stream_snapshot(&mut state.stream_all, &owner_url, new_rows);
            let label_pending = state
                .stream_pending
                .iter()
                .any(|owner| state.stream_owner_labels.get(owner).unwrap_or(owner) == &addon);
            if complete && !label_pending && !state.stream_all.iter().any(|row| row.addon == addon)
            {
                // Returned nothing: remove its pill and fall back to All if it
                // was the active filter.
                if let Some(pos) = state.stream_addons.iter().position(|a| a == &addon) {
                    state.stream_addons.remove(pos);
                    state.stream_filter = selected_label
                        .as_ref()
                        .and_then(|selected| {
                            state
                                .stream_addons
                                .iter()
                                .position(|label| label == selected)
                        })
                        .map_or(0, |index| index + 1);
                }
            }
            !state.stream_pending.is_empty()
        };

        self.apply_stream_filter();
        if let Some(app) = self.app() {
            app.set_streams_searching(searching);
        }
        self.publish_stream_refresh_gate();
        if !searching {
            self.season_download_list_ready();
        }
    }

    /// Build the displayed stream list from `stream_all`, narrowed by the
    /// active per-addon filter: the pinned download rows for this request
    /// first, then the filtered addon rows, plus the status hint.
    fn compute_stream_view(&self) -> (Vec<StreamUi>, Vec<StreamRow>, String) {
        let (
            filtered,
            filter,
            addon_count,
            total,
            pending_count,
            addon_label,
            request_id,
            media_type,
            media_id,
            series_id,
        ) = {
            let mut state = self.shared.lock().unwrap();
            refresh_stream_owner_order(&mut state);
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
                .filter(|s| {
                    s.download.as_ref().map_or_else(
                        || {
                            profile_policy::allows_addon(
                                &profiles::ProfileStore::capture(),
                                &s.owner_url,
                            )
                        },
                        |j| smart_offline::allows_download(&profiles::ProfileStore::capture(), j),
                    )
                })
                .filter(|s| filter == 0 || selected.as_deref().is_some_and(|a| a == s.addon))
                .cloned()
                .collect();
            // Establish a stable tie order independent of response timing.
            // Universal rules rank the combined list across owners below.
            if !state.stream_owner_order.is_empty() {
                sort_streams_by_owner(&mut filtered, &state.stream_owner_order);
            } else if filter == 0 {
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
                state
                    .modal_item
                    .as_ref()
                    .map(|m| m.type_.clone())
                    .unwrap_or_default(),
                state
                    .modal_item
                    .as_ref()
                    .map(|m| m.id.clone())
                    .unwrap_or_default(),
                state
                    .modal_item
                    .as_ref()
                    .and_then(|m| stream_series_scope(&m.type_, &m.id)),
            )
        };

        let mut hint = if pending_count > 0 {
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
        let warnings = self
            .shared
            .lock()
            .unwrap()
            .stream_warnings
            .values()
            .flat_map(|rows| rows.iter())
            .map(|(source, message)| {
                if source.is_empty() {
                    text::tr(message).to_owned()
                } else {
                    format!("{source}: {}", text::tr(message))
                }
            })
            .collect::<Vec<_>>();
        if !warnings.is_empty() {
            hint.push('\n');
            hint.push_str(&warnings.join("\n"));
        }
        let mut filtered = filtered;
        if !active_torrent_settings().enabled {
            filtered.retain(|stream| !matches!(stream.source, StreamSource::Torrent { .. }));
        }
        let rules = self.effective_stream_filter_rules();
        let hidden = stream_filters::apply_stream_rules(&mut filtered, &rules);
        if let Some(app) = self.app() {
            app.global::<crate::StreamFiltersBridge>()
                .set_hidden_count(hidden.min(i32::MAX as usize) as i32);
        }
        if hidden > 0 {
            if filtered.is_empty() && pending_count == 0 {
                hint = text::tr(
                    "No addon streams match your rules. Change them in Settings → Player.",
                )
                .to_owned();
            } else {
                hint.push('\n');
                hint.push_str(&text::stream_rules_hidden(hidden));
            }
        }
        let mut displayed: Vec<StreamUi> = self
            .downloads
            .jobs_for_request(&request_id)
            .into_iter()
            .filter(|job| job.media_type == media_type && job.media_id == media_id)
            .map(|job| {
                let path = job.artifact_path.clone().unwrap_or_default();
                StreamUi {
                    id: format!("download:{}", job.id),
                    choice_id: Some(stream_choice_identity(
                        "",
                        &media_type,
                        &request_id,
                        &addons::Stream {
                            url: Some(format!("nova-download:{}", job.id)),
                            ..Default::default()
                        },
                    )),
                    owner_url: String::new(),
                    display: job.display.clone(),
                    card: downloaded_stream_card(&job),
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
        if self.shared.lock().unwrap().season_download.is_some() {
            // Planning offers downloadable files only. Browser handoffs and
            // existing jobs must never look like selectable episode sources.
            displayed.retain(season_downloads::downloadable);
        }
        let last_used = self.last_stream.identity(nova_config::now_secs());
        let has_exact = last_used.as_deref().is_some_and(|id| {
            displayed.iter().any(|row| {
                stream_recommendation_usable(row) && row.choice_id.as_deref() == Some(id)
            })
        });
        let recommended_id = if has_exact {
            None
        } else {
            series_id
                .as_deref()
                .and_then(|id| {
                    self.last_stream
                        .series_preference(id, nova_config::now_secs())
                })
                .and_then(|preference| {
                    recommended_stream(&displayed, last_used.as_deref(), &preference)
                })
                .and_then(|index| displayed[index].choice_id.clone())
        };
        promote_last_stream(
            &mut displayed,
            last_used
                .as_deref()
                .filter(|_| has_exact)
                .or(recommended_id.as_deref()),
        );
        // Apply the complete user order after remembered-family promotion,
        // across every addon. Only true comparator ties retain that promotion.
        stream_filters::rank_stream_rules(&mut displayed, &rules);
        let last_used_index = last_used.as_deref().and_then(|id| {
            displayed.iter().position(|row| {
                stream_recommendation_usable(row) && row.choice_id.as_deref() == Some(id)
            })
        });
        let (party_room, party_media) = {
            let state = self.shared.lock().unwrap();
            (
                state.party.snapshot_for_sources(),
                state.modal_item.as_ref().map(|m| {
                    party_source::canonical_media(
                        nova_playback_api::MediaIdentity {
                            media_type: m.type_.clone(),
                            title_id: m.id.clone(),
                            episode_id: m.request_id.clone(),
                            ..Default::default()
                        },
                        &m.videos,
                    )
                }),
            )
        };
        let stream_rows: Vec<StreamRow> = displayed
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let details = if matches!(row.source, StreamSource::ExternalUrl(_)) {
                    text::tr("Open in browser").to_string()
                } else {
                    row.download
                        .as_ref()
                        .map(DownloadCoordinator::status_text)
                        .unwrap_or_default()
                };
                let lines = Self::estimate_stream_lines(&row.display)
                    + if details.is_empty() { 0 } else { 1 };
                StreamRow {
                    id: SharedString::from(&row.id),
                    reveal_new: false,
                    text: SharedString::from(&row.display),
                    title: SharedString::from(&row.card.title),
                    metadata: SharedString::from(&row.card.metadata),
                    description: SharedString::from(&row.card.description),
                    size: SharedString::from(&row.card.size),
                    details: SharedString::from(details),
                    lines,
                    is_download: row.download.is_some(),
                    external_link: matches!(row.source, StreamSource::ExternalUrl(_)),
                    last_used: last_used_index == Some(index),
                    party_recommended: party_media.as_ref().is_some_and(|media| {
                        party_source::recommended(party_room.as_ref(), media, &row.source)
                    }),
                    recommended: recommended_id
                        .as_deref()
                        .is_some_and(|id| row.choice_id.as_deref() == Some(id)),
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
    /// `set_row_data`, which preserves the delegates and their timers. New
    /// addon batches insert/remove by stable ID so retained rows also keep
    /// their reveal state instead of fading out again with every response.
    pub(super) fn apply_stream_filter(&self) {
        self.apply_stream_filter_with_focus(true);
    }

    fn apply_stream_filter_with_focus(&self, preserve_focus: bool) {
        if self.remote_output_selected() {
            return;
        }
        let Some(app) = self.app() else {
            return;
        };
        // Touch/pointer browsing has no selected keyboard row. Following the
        // stored cursor would skip higher-priority results as they arrive and
        // move to the old row's new page, despite correctly sorted All rows.
        let preserve_focus = preserve_focus && app.get_kb_active();
        let previous_focus = app.get_detail_kb_s().max(0) as usize;
        let focused_id = if preserve_focus {
            self.shared
                .lock()
                .unwrap()
                .streams
                .get(previous_focus)
                .map(|row| row.id.clone())
        } else {
            None
        };
        let (displayed, stream_rows, hint) = self.compute_stream_view();
        // Reorder pills alongside All's rows, preserving the selected addon
        // by identity. Avoid rebuilding unchanged models on progress ticks.
        let (addons, loading, filter) = {
            let state = self.shared.lock().unwrap();
            let addons: Vec<SharedString> =
                state.stream_addons.iter().map(SharedString::from).collect();
            let loading: Vec<bool> = state
                .stream_addons
                .iter()
                .map(|label| {
                    state
                        .stream_pending
                        .iter()
                        .any(|owner| state.stream_owner_labels.get(owner).unwrap_or(owner) == label)
                })
                .collect();
            (addons, loading, state.stream_filter)
        };
        if !app.get_stream_addons().iter().eq(addons.iter().cloned()) {
            app.set_stream_addons(Rc::new(VecModel::from(addons)).into());
        }
        if !app.get_stream_loading().iter().eq(loading.iter().copied()) {
            app.set_stream_loading(Rc::new(VecModel::from(loading)).into());
        }
        app.set_stream_filter(filter as i32);
        let focused_index = focused_id
            .as_ref()
            .and_then(|id| displayed.iter().position(|row| &row.id == id))
            // A replacement can remove the focused row. Keep Enter usable by
            // falling back to the nearest remaining index, including its page.
            .or_else(|| {
                preserve_focus.then(|| previous_focus.min(displayed.len().saturating_sub(1)))
            });
        if app.get_stream_action_open()
            && !displayed
                .iter()
                .any(|row| row.id == app.get_stream_action_id().as_str())
        {
            app.set_stream_action_open(false);
        }
        let (page, page_count, page_start, total, page_rows) = {
            let mut state = self.shared.lock().unwrap();
            let page_count = stream_rows.len().div_ceil(STREAM_PAGE_SIZE).max(1);
            if let Some(index) = focused_index {
                state.stream_page = index / STREAM_PAGE_SIZE;
            }
            state.stream_page = state.stream_page.min(page_count - 1);
            let page = state.stream_page;
            let start = page * STREAM_PAGE_SIZE;
            let end = (start + STREAM_PAGE_SIZE).min(stream_rows.len());
            state.streams = displayed;
            state.stream_hint = None;
            let page_rows = stream_rows[start..end]
                .iter()
                .cloned()
                .map(|mut row| {
                    row.reveal_new = state.stream_revealed_ids.insert(row.id.to_string());
                    row
                })
                .collect::<Vec<_>>();
            (page, page_count, start, stream_rows.len(), page_rows)
        };
        app.set_stream_page(page as i32);
        app.set_stream_page_count(page_count as i32);
        app.set_stream_page_start(page_start as i32);
        app.set_stream_total(total as i32);
        if let Some(index) = focused_index {
            app.set_detail_kb_s(index as i32);
        }
        let model = app.get_streams();
        let existing = model.as_any().downcast_ref::<VecModel<StreamRow>>();
        if let Some(model) = existing {
            update_stream_rows(model, &page_rows);
        } else {
            app.set_streams(Rc::new(VecModel::from(page_rows)).into());
        }
        app.set_streams_hint(SharedString::from(hint));
    }

    /// Move the visible stream list to an absolute (zero-based) page.
    pub(super) fn stream_page_picked(&self, page: usize) {
        if self.remote_pick_page(page) {
            return;
        }
        {
            let mut state = self.shared.lock().unwrap();
            state.stream_page = page;
        }
        self.apply_stream_filter_with_focus(false);
    }

    /// A filter pill was tapped (0 = All, else 1-based addon index).
    pub(super) fn stream_filter_picked(&self, index: usize) {
        if self.remote_pick_filter(index) {
            return;
        }
        {
            let mut state = self.shared.lock().unwrap();
            if index <= state.stream_addons.len() {
                state.stream_filter = index;
                state.stream_page = 0;
            } else {
                return;
            }
        }
        self.apply_stream_filter_with_focus(false);
        if let Some(app) = self.app() {
            app.set_stream_filter(index as i32);
            app.set_detail_kb_s(0);
        }
    }

    /// Clear the stream list and its addon-filter state (state + UI).
    pub(super) fn clear_streams(&self) {
        self.remote_disconnect();
        self.cancel_pending_for_navigation();
        {
            let mut state = self.shared.lock().unwrap();
            state.stream_generation = state.stream_generation.wrapping_add(1);
            state.stream_revealed_ids.clear();
            state.stream_owner_labels.clear();
            state.stream_owner_order.clear();
            state.stream_cached_owners.clear();
            state.streams.clear();
            state.stream_all.clear();
            state.stream_addons.clear();
            state.stream_pending.clear();
            state.stream_warnings.clear();
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
        self.publish_stream_refresh_gate();
    }

    pub(super) fn stream_picked(&self, index: usize) {
        {
            let state = self.shared.lock().unwrap();
            let owner = profiles::ProfileStore::capture();
            if state.streams.get(index).is_none_or(|s| {
                !s.download.as_ref().map_or_else(
                    || profile_policy::allows_addon(&owner, &s.owner_url),
                    |j| smart_offline::allows_download(&owner, j),
                )
            }) || state
                .modal_item
                .as_ref()
                .is_some_and(|m| !profile_policy::allows_title(&owner, &m.type_, &m.id))
            {
                return;
            }
        }
        if self.season_download_pick(index) {
            return;
        }
        let browser_url = {
            let state = self.shared.lock().unwrap();
            state.streams.get(index).and_then(|row| match &row.source {
                StreamSource::ExternalUrl(url) => Some(url.clone()),
                _ => None,
            })
        };
        if let Some(url) = browser_url {
            if self.shared.lock().unwrap().casting.selected {
                self.set_stream_hint(Some(StreamHint::Fixed(
                    "Browser links cannot be cast. Choose a direct media source.",
                )));
                return;
            }
            if detail_extras::web_url(&url).is_none() || crate::player::open_browser(&url).is_err()
            {
                self.set_stream_hint(Some(StreamHint::Fixed(
                    "Could not open this link in your browser.",
                )));
            }
            return;
        }
        if self.remote_stream_selected(index) {
            return;
        }
        let stream = {
            let state = self.shared.lock().unwrap();
            state.streams.get(index).cloned()
        };
        let stream = match stream {
            Some(s) => s,
            None => return,
        };
        if !matches!(stream.source, StreamSource::Downloaded { .. })
            && !self
                .effective_stream_filter_rules()
                .accepts(&stream.card.filter_facts)
        {
            self.set_stream_hint(Some(StreamHint::Fixed(
                "This source no longer matches your stream rules.",
            )));
            return;
        }
        if !self
            .shared
            .lock()
            .unwrap()
            .remote_playback
            .can_play_locally()
        {
            return;
        }
        // Capture the chosen source before last-used promotion can reorder
        // pointer rows or later addon replies move the keyboard cursor.
        self.note_selected_playback_stream(&stream);
        if let Some(app) = self.app() {
            app.set_detail_kb_s(index as i32);
        }

        // A failed/unfinished download and an unsupported row are not choices
        // the user can use. Save the original resolver identity before it is
        // exchanged for a temporary playback URL.
        let usable = match &stream.source {
            StreamSource::Unsupported | StreamSource::ExternalUrl(_) => false,
            StreamSource::Downloaded { job_id, path } => {
                path.is_file()
                    && self
                        .downloads
                        .job(job_id)
                        .is_some_and(|job| job.phase == crate::download::DownloadPhase::Completed)
            }
            _ => true,
        };
        let recovering = self
            .shared
            .lock()
            .unwrap()
            .playback_tools
            .pending_retry
            .as_ref()
            .is_some_and(|retry| retry.context.automatic_source_selection);
        if usable
            && !recovering
            && let Some(identity) = &stream.choice_id
        {
            self.last_stream
                .remember(identity.clone(), nova_config::now_secs());
            if stream.download.is_none() {
                let series_id = self
                    .shared
                    .lock()
                    .unwrap()
                    .modal_item
                    .as_ref()
                    .and_then(|item| stream_series_scope(&item.type_, &item.id));
                if let Some(series_id) = series_id {
                    self.last_stream.remember_series(
                        &series_id,
                        StreamPreference::from_row(&stream),
                        nova_config::now_secs(),
                    );
                }
            }
            // Publish the badge before playback, so this refresh cannot
            // overwrite a subsequent resolver/playback failure hint.
            self.apply_stream_filter();
        }

        self.shared.lock().unwrap().pending_subtitle_hints = if usable {
            stream.card.subtitle_hints.clone()
        } else {
            Vec::new()
        };
        match stream.source.clone() {
            StreamSource::Url(url) => {
                // Android can hand the URL to another video app; desktop
                // always uses the built-in player, including saved preferences.
                if nova_config::active_cache_settings().uses_external_player()
                    && !self.recovery_source_selected()
                {
                    self.begin_playback_request();
                    let _ = crate::player::open_external(&url);
                } else if !self.open_player(url.clone())
                    && (self.recovery_source_selected()
                        || crate::player::open_external(&url).is_err())
                {
                    self.set_stream_hint(Some(StreamHint::Fixed(
                        "This stream requires the in-app player, which is unavailable.",
                    )));
                }
            }
            StreamSource::UrlWithOptions {
                url,
                headers,
                subtitles,
            } => {
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
            StreamSource::Resolver {
                provider_url,
                token,
                settings_generation,
                addon_generation,
            } => {
                self.play_provider_resolver(
                    provider_url,
                    token,
                    settings_generation,
                    addon_generation,
                );
            }
            StreamSource::ExternalUrl(_) => {} // Handled before any playback effects above.
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
                    self.play_download(&job_id);
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
        self.refresh_watch_action();
        let Some(app) = self.app() else { return };
        if !app.get_modal_visible() {
            return;
        }

        let seasons = {
            let state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_ref() else {
                return;
            };
            catalog_detail::seasons(&state, modal)
        };
        self.publish_catalog_detail_scope();
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

        let (_, _, page_rows) = self.current_episode_page();
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

/// Prefetch and the visible picker must use identical ownership and alias
/// routing. Building a successor lookup does not alter the opened Detail item.
pub(super) fn stream_search_urls_for(
    state: &Shared,
    request_id: &str,
) -> Vec<(String, String, String, u64)> {
    let Some(modal) = state.modal_item.as_ref() else {
        return vec![];
    };
    stream_search_urls_for_context(&state.installed, modal, request_id)
}

/// Independent lookup context: remote searches never replace the visible Detail.
pub(super) fn stream_search_urls_for_context(
    installed: &[Installed],
    modal: &ModalItem,
    request_id: &str,
) -> Vec<(String, String, String, u64)> {
    let owner = profiles::ProfileStore::capture();
    if !profile_policy::allows_title(&owner, &modal.type_, &modal.id) {
        return vec![];
    }
    let lookup = source_stream_lookup(modal, request_id);
    let stream_ids = episode_stream_ids(modal, request_id);
    installed
        .iter()
        .filter(|a| profile_policy::allows_addon(&owner, &a.url))
        .filter_map(|a| {
            let video = modal.videos.iter().find(|v| v.id == request_id);
            let owner = video
                .and_then(|v| v.extra.get("novaSourceUrl"))
                .and_then(serde_json::Value::as_str);
            let aliases = stream_ids
                .iter()
                .filter(|id| {
                    if public_stream_id(id) {
                        return true;
                    }
                    video
                        .and_then(|v| v.extra.get("novaStreamIds"))
                        .and_then(serde_json::Value::as_array)
                        .and_then(|ids| {
                            ids.iter()
                                .position(|value| value.as_str() == Some(id.as_str()))
                        })
                        .and_then(|i| video?.extra.get("novaConnections")?.as_array()?.get(i))
                        .and_then(|c| c.get("addonId"))
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|id| id == nova_providers::addon_metadata_id(&a.url))
                })
                .cloned()
                .collect::<Vec<_>>();
            stream_endpoint_owned(
                a,
                &modal.type_,
                request_id,
                lookup.as_ref(),
                &aliases,
                owner,
            )
            .map(|url| (a.label.clone(), url, a.url.clone(), a.generation))
        })
        .collect()
}

fn stream_response_is_current(state: &Shared, request_id: &str, generation: u64) -> bool {
    state.stream_generation == generation
        && state
            .modal_item
            .as_ref()
            .is_some_and(|modal| modal.request_id == request_id)
}

pub(crate) fn public_stream_id(id: &str) -> bool {
    if nova_providers::ExternalId::parse(id).is_some() {
        return true;
    }
    let parts = id.split(':').collect::<Vec<_>>();
    // Season zero is the standard public identity for specials. Requiring a
    // positive season makes another addon's metadata look like a private ID.
    let unsigned = |number: &str| {
        !number.is_empty()
            && number.bytes().all(|c| c.is_ascii_digit())
            && number.parse::<u32>().is_ok()
    };
    let positive = |number: &str| unsigned(number) && number.parse::<u32>().is_ok_and(|n| n > 0);
    match parts.as_slice() {
        [head, season, episode] if nova_providers::ExternalId::parse(head).is_some() => {
            unsigned(season) && positive(episode)
        }
        ["tmdb" | "mal" | "anilist", number] => positive(number),
        ["tmdb" | "mal" | "anilist", number, season, episode] => {
            positive(number) && unsigned(season) && positive(episode)
        }
        _ => false,
    }
}

#[cfg(test)]
fn stream_endpoint(
    addon: &Installed,
    media_type: &str,
    request_id: &str,
    lookup: Option<&nova_providers::StreamLookupRequest>,
    stream_ids: &[String],
) -> Option<String> {
    stream_endpoint_owned(addon, media_type, request_id, lookup, stream_ids, None)
}

fn stream_endpoint_owned(
    addon: &Installed,
    media_type: &str,
    request_id: &str,
    lookup: Option<&nova_providers::StreamLookupRequest>,
    stream_ids: &[String],
    owner: Option<&str>,
) -> Option<String> {
    if !addon.enabled || !addon.manifest.has_streams() {
        return None;
    }
    // Contextual resolution is declared by the provider. Its own
    // manifest restrictions continue to protect normal meta/stream routing.
    if nova_providers::supports_contextual_streams(&addon.url)
        && !nova_providers::provider_owns_id(&addon.url, request_id)
    {
        return nova_providers::stream_lookup_url(&addon.url, lookup?).ok();
    }
    let target_id = if !nova_providers::provider_owns_id(&addon.url, request_id)
        && (nova_providers::private_provider_id(request_id)
            || owner.is_some_and(|url| url != addon.url && !public_stream_id(request_id)))
    {
        // A source episode keeps its own library identity. Only the outbound
        // addon request uses a canonical episode ID confirmed by the mapper.
        stream_ids
            .iter()
            .find(|id| addon.manifest.accepts("stream", media_type, id))
            .map(String::as_str)?
    } else if addon.manifest.accepts("stream", media_type, request_id) {
        request_id
    } else {
        stream_ids
            .iter()
            .find(|id| addon.manifest.accepts("stream", media_type, id))
            .map(String::as_str)?
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
    modal
        .videos
        .iter()
        .find(|video| video.id == request_id)
        .map(confirmed_episode_stream_ids)
        .unwrap_or_default()
}

/// Metadata's confirmed aliases are shared by stream routing and Library
/// migration. Old inventory revisions must never become migration evidence.
pub(crate) fn confirmed_episode_stream_ids(video: &Video) -> Vec<String> {
    if video
        .extra
        .get("novaMetadataRevision")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|revision| revision != nova_providers::metadata_revision())
    {
        return Vec::new();
    }
    video
        .extra
        .get("novaStreamIds")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .take(16)
        .filter_map(serde_json::Value::as_str)
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 1024
                && !nova_providers::private_provider_id(id)
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
    for video in existing.iter_mut() {
        let Some(updated) = fresh.get(video.id.as_str()) else {
            continue;
        };
        for key in [
            "novaStreamIds",
            "novaConnections",
            "novaMetadataRevision",
            "novaSourceUrl",
        ] {
            if let Some(value) = updated.extra.get(key) {
                video.extra.insert(key.into(), value.clone());
            } else {
                video.extra.remove(key);
            }
        }
    }
}

fn source_stream_lookup(
    modal: &ModalItem,
    request_id: &str,
) -> Option<nova_providers::StreamLookupRequest> {
    if modal.type_ == "movie" && modal.id == request_id {
        return Some(nova_providers::StreamLookupRequest {
            media_id: modal.id.clone(),
            media_type: modal.type_.clone(),
            title: modal.name.clone(),
            year: (!modal.year.is_empty()).then(|| modal.year.clone()),
            season: None,
            episode: None,
            absolute_episode: None,
            season_episode_count: None,
            series_episode_count: None,
            episode_title: None,
            released: None,
        });
    }
    if modal.type_ != "series" {
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
        season: Some(season),
        episode: Some(episode),
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
            details: Default::default(),
            open_token: Arc::new(()),
            pending_watch_now: None,
            episodes_loading: false,
            id: "tt0994314".into(),
            type_: "series".into(),
            request_id: "foreign-opaque-episode".into(),
            videos,
            season_backdrops: HashMap::new(),
            seasons: vec![1, 2],
            season_index: 1,
            episode_page: 0,
            name: "Code Geass".into(),
            year: "2006–2008".into(),
            poster_url: String::new(),
            background_url: String::new(),
            logo_url: String::new(),
            description: String::new(),
            genres: Vec::new(),
        }
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn detail_progress_repaint_retains_models_artwork_and_thumbnail_policy() {
        const ROOT: &str = "NOVA_DETAIL_PROGRESS_REPAINT_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-detail-progress-repaint-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root).env("XDG_CACHE_HOME", root.join("cache"))
                .args(["--exact", "app::detail::source_lookup_tests::detail_progress_repaint_retains_models_artwork_and_thumbnail_policy", "--nocapture"])
                .output().unwrap();
            let _ = fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        storage::init_at(&root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let (hi, _) = mpsc::channel();
        let (lo, _) = mpsc::channel();
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        let mut selected = modal();
        selected.videos.truncate(2);
        selected.seasons = vec![1];
        selected.season_index = 0;
        for (index, video) in selected.videos.iter_mut().enumerate() {
            video.thumbnail = Some(format!("https://example.test/thumb-{index}"));
        }
        let series = selected.id.clone();
        let ids = selected
            .videos
            .iter()
            .map(|video| video.id.clone())
            .collect::<Vec<_>>();
        let urls = selected
            .videos
            .iter()
            .map(|video| video.thumbnail.clone().unwrap())
            .collect::<Vec<_>>();
        let pixels = SharedPixelBuffer::<Rgba8Pixel>::new(64, 36);
        for url in &urls {
            nova_media::cache::decoded_cache_insert(
                &image_cache_key(url, ImageUse::EpisodeThumbnail),
                pixels.clone(),
            );
        }
        {
            let mut state = bridge.shared.lock().unwrap();
            state.cache_settings.show_unwatched_thumbs = false;
            state.modal_item = Some(selected);
            state.progress.insert(
                progress_map_key(&series, &ids[0]),
                EpisodeProgress {
                    series_id: series.clone(),
                    episode_id: ids[0].clone(),
                    position_secs: 300.0,
                    duration_secs: 1200.0,
                    ..Default::default()
                },
            );
        }
        app.set_episode_rows(Rc::new(VecModel::from(bridge.current_episode_page().2)).into());
        app.set_season_cards(Rc::new(VecModel::from(bridge.current_season_cards())).into());
        let episode_model = app.get_episode_rows();
        let season_model = app.get_season_cards();
        let first_image = episode_model.row_data(0).unwrap().thumb;
        assert!(episode_model.row_data(0).unwrap().has_thumb);
        assert!(!episode_model.row_data(1).unwrap().has_thumb);
        // A displayed image survives LRU eviction; the newly revealed second
        // image is found only in memory, without scheduling a thumbnail fetch.
        nova_media::cache::decoded_cache_clear();
        nova_media::cache::decoded_cache_insert(
            &image_cache_key(&urls[1], ImageUse::EpisodeThumbnail),
            pixels,
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            for id in &ids {
                Bridge::set_episode_watched_locked(&mut state.progress, &series, id, true);
            }
        }
        bridge.refresh_detail_progress_ui();
        assert!(app.get_episode_rows() == episode_model);
        assert!(app.get_season_cards() == season_model);
        assert!(episode_model.iter().all(|row| row.watched && row.has_thumb));
        assert_eq!(episode_model.row_data(0).unwrap().thumb, first_image);
        assert!(
            !episode_model
                .row_data(0)
                .unwrap()
                .details
                .contains("Resume")
        );
        assert_eq!(season_model.row_data(0).unwrap().progress, 1.0);
        assert!(season_model.row_data(0).unwrap().watched);
        nova_media::cache::decoded_cache_clear();
        bridge.refresh_detail_progress_ui();
        assert!(
            episode_model.iter().all(|row| row.has_thumb),
            "displayed pixels survive eviction"
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            for id in &ids {
                Bridge::set_episode_watched_locked(&mut state.progress, &series, id, false);
            }
        }
        bridge.refresh_detail_progress_ui();
        assert!(
            episode_model
                .iter()
                .all(|row| !row.watched && !row.has_thumb && row.artwork_identity.is_empty())
        );
        assert_eq!(season_model.row_data(0).unwrap().progress, 0.0);
        bridge
            .shared
            .lock()
            .unwrap()
            .progress
            .get_mut(&progress_map_key(&series, &ids[0]))
            .unwrap()
            .position_secs = 300.0;
        bridge.refresh_detail_progress_ui();
        let resumed = episode_model.row_data(0).unwrap();
        assert!(
            !resumed.has_thumb,
            "uncached newly visible images stay placeholders"
        );
        assert!(!resumed.artwork_identity.is_empty());
        assert!(resumed.details.contains("Resume"));
        assert!(app.get_episode_rows() == episode_model);
        assert!(
            EPISODE_QUEUE.lock().unwrap().is_empty(),
            "progress repaint does not queue artwork"
        );
    }

    /// Measure the real manual action rather than rendering/readback time.
    /// The fixture includes cached offscreen series and a real ordered writer;
    /// timings are informational and do not impose machine-specific budgets.
    #[test]
    #[cfg(feature = "desktop")]
    #[ignore = "informational full season-action callback timings"]
    fn measure_season_watch_action_callback_cost() {
        const ROOT: &str = "NOVA_SEASON_ACTION_TIMING_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-season-action-timing-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .env("XDG_CACHE_HOME", root.join("cache"))
                .args([
                    "--exact",
                    "app::detail::source_lookup_tests::measure_season_watch_action_callback_cost",
                    "--ignored",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            let _ = fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            print!("{}", String::from_utf8_lossy(&output.stdout));
            return;
        };
        let root = PathBuf::from(root);
        storage::init_at(&root);
        i_slint_backend_testing::init_integration_test_with_system_time();
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let (hi, _) = mpsc::channel();
        let (lo, _) = mpsc::channel();
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        let mut entries = Vec::new();
        let mut progress = HashMap::new();
        let mut selected = modal();
        selected.seasons = vec![1, 2, 3, 4, 5];
        selected.season_index = 0;
        selected.episode_page = 0;
        for series in 0..60 {
            let id = format!("timing-series-{series}");
            let videos = (1..=100)
                .map(|number| {
                    let season = (number - 1) / 20 + 1;
                    let episode = (number - 1) % 20 + 1;
                    let video = Video {
                        id: format!("{id}:{season}:{episode}"),
                        name: format!("Episode {episode}"),
                        season: Some(season),
                        episode: Some(episode),
                        released: Some(
                            if season == 1 && episode == 20 {
                                "2999-01-01"
                            } else {
                                "2020-01-01"
                            }
                            .into(),
                        ),
                        overview: Some(
                            "A cached episode synopsis for the season action fixture.".into(),
                        ),
                        ..Default::default()
                    };
                    progress.insert(
                        progress_map_key(&id, &video.id),
                        EpisodeProgress {
                            series_id: id.clone(),
                            episode_id: video.id.clone(),
                            position_secs: if number == 1 { 300.0 } else { 0.0 },
                            duration_secs: 1200.0,
                            updated_at_secs: 1,
                            ..Default::default()
                        },
                    );
                    video
                })
                .collect::<Vec<_>>();
            write_episodes_cache_for("series", &id, &videos);
            entries.push(
                serde_json::from_value::<LibraryEntry>(serde_json::json!({
                    "id": id, "type_": "series", "name": format!("Timing series {series}")
                }))
                .unwrap(),
            );
            if series == 0 {
                selected.id = id.clone();
                selected.request_id = videos[0].id.clone();
                selected.videos = videos;
            }
        }
        {
            let mut state = bridge.shared.lock().unwrap();
            state.entries = entries;
            state.progress = progress;
            state.modal_item = Some(selected);
        }
        let _writer = persistence::start().expect("fixture persistence writer");
        bridge.persist_library();
        bridge.migrate_viewing_activity();
        bridge.apply_library_to_ui();
        bridge.apply_episode_rows();
        bridge.apply_season_cards();
        persistence::flush().unwrap();
        app.set_show_home(false);
        app.set_modal_visible(true);
        app.set_detail_tab(3);
        for round in 0..6 {
            let watched = round % 2 == 0;
            let episodes_before = app.get_episode_rows();
            let seasons_before = app.get_season_cards();
            let started = std::time::Instant::now();
            bridge.season_watch_action(0, if watched { 1 } else { 2 });
            let elapsed = started.elapsed();
            assert_eq!(app.get_episode_rows().row_data(0).unwrap().watched, watched);
            assert!(
                !app.get_episode_rows().row_data(19).unwrap().watched,
                "future episode is untouched"
            );
            assert_eq!(
                app.get_season_cards().row_data(0).unwrap().progress,
                if watched { 19.0 / 20.0 } else { 0.0 }
            );
            let episodes_retained =
                std::ptr::eq(episodes_before.as_any(), app.get_episode_rows().as_any());
            let seasons_retained =
                std::ptr::eq(seasons_before.as_any(), app.get_season_cards().as_any());
            println!(
                "SEASON_ACTION_CALLBACK series=60 episodes=6000 round={round} watched={watched} elapsed_us={} episode_model_retained={episodes_retained} season_model_retained={seasons_retained}",
                elapsed.as_micros()
            );
            persistence::flush().unwrap();
        }
        // Include the final prepared UI publication, not just the fast manual
        // callback. System time lets real workers and the event loop complete.
        let before = progress_projection::publication_count();
        let requested = std::time::Instant::now();
        bridge.season_watch_action(0, 1);
        let timer = slint::Timer::default();
        let published = Rc::new(std::cell::Cell::new(false));
        let completed = published.clone();
        let weak = app.as_weak();
        let final_bridge = bridge.clone();
        timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(5), move || {
            assert!(requested.elapsed() < std::time::Duration::from_secs(30), "prepared progress publication timed out");
            if progress_projection::publication_count() <= before { return; }
            let app = weak.upgrade().unwrap();
            let card = app.get_library().iter().find(|card| card.id == "timing-series-0").expect("published target Library card");
            assert_eq!(card.watched_count, 19);
            assert!(app.get_home_continue().iter().any(|row| row.id == "timing-series-0"), "accepted publication includes current Home continuation");
            assert!(!app.get_episode_rows().row_data(19).unwrap().watched);
            let state = final_bridge.shared.lock().unwrap();
            let continuing = state.continue_list.iter().find(|row| row.series_id == "timing-series-0").expect("prepared continuation installed");
            assert!(!state.progress.get(&progress_map_key("timing-series-0", &continuing.episode_id)).is_some_and(|progress| progress.watched));
            println!("SEASON_ACTION_PUBLICATION series=60 episodes=6000 callback_us={} ready_after_us={}", progress_projection::last_publication_micros(), requested.elapsed().as_micros());
            completed.set(true);
            slint::quit_event_loop().unwrap();
        });
        slint::run_event_loop().unwrap();
        assert!(published.get());
        persistence::flush().unwrap();
    }

    #[test]
    fn resized_player_preview_rejects_new_artwork_requests_settings_and_reopened_titles() {
        let mut state = Shared {
            modal_item: Some(modal()),
            stream_generation: 12,
            ..Default::default()
        };
        let modal = state.modal_item.as_ref().unwrap();
        let request = EpisodePreviewRequest {
            open_token: modal.open_token.clone(),
            episode_id: modal.request_id.clone(),
            stream_generation: 12,
            image_revision: 4,
        };
        assert!(request.accepts(&state, 4, false));
        assert!(
            !request.accepts(&state, 4, true),
            "a recipe fetch already won"
        );
        assert!(!request.accepts(&state, 5, false), "image policy changed");
        state.stream_generation += 1;
        assert!(
            !request.accepts(&state, 4, false),
            "same episode was requested again"
        );
        state.stream_generation = 12;
        state.modal_item.as_mut().unwrap().request_id = "next-episode".into();
        assert!(!request.accepts(&state, 4, false));
        state.modal_item.as_mut().unwrap().request_id = request.episode_id.clone();
        assert!(request.accepts(&state, 4, false));
        state.modal_item.as_mut().unwrap().open_token = Arc::new(());
        assert!(
            !request.accepts(&state, 4, false),
            "same title was reopened"
        );
        state.modal_item = None;
        assert!(!request.accepts(&state, 4, false), "Detail closed");
    }

    #[test]
    fn movie_context_uses_authoritative_title_without_episode_fields() {
        let mut modal = modal();
        modal.type_ = "movie".into();
        modal.id = "tt1727587".into();
        modal.request_id = modal.id.clone();
        modal.name = "Sintel".into();
        modal.year = "2010".into();
        modal.videos.clear();
        let lookup = source_stream_lookup(&modal, &modal.request_id).unwrap();
        assert_eq!(lookup.media_id, "tt1727587");
        assert_eq!(lookup.title, "Sintel");
        assert!(lookup.season.is_none() && lookup.episode.is_none());
        let _packages = super::super::tests::provider_package_fixture_guard();
        let provider = super::super::tests::provider_package_fixture("supernova");
        let addon = Installed {
            url: provider.url(),
            label: "Supernova".into(),
            enabled: true,
            configure_ok: Some(false),
            manifest: provider.addon_manifest().clone(),
            available: true,
            generation: 1,
        };
        assert!(
            stream_endpoint(&addon, "movie", &modal.request_id, Some(&lookup), &[])
                .unwrap()
                .starts_with("nova-provider://supernova/resolve/movie/")
        );
        assert!(source_stream_lookup(&modal, "some-other-movie").is_none());
    }

    #[test]
    fn foreign_library_episode_uses_contextual_source_route_and_keeps_original_ids() {
        let _packages = super::super::tests::provider_package_fixture_guard();
        let modal = modal();
        let lookup = source_stream_lookup(&modal, &modal.request_id).unwrap();
        assert_eq!(lookup.media_id, "tt0994314");
        assert_eq!(lookup.season, Some(2));
        assert_eq!(lookup.episode, Some(1));
        assert_eq!(lookup.absolute_episode, Some(26));
        assert_eq!(lookup.season_episode_count, Some(1));
        assert_eq!(lookup.series_episode_count, Some(26));
        let mut addon = Installed {
            url: nova_providers::ANIKOTO_PROVIDER_URL.into(),
            label: "AniKoto".into(),
            enabled: true,
            configure_ok: Some(false),
            manifest: super::super::tests::provider_package_fixture("anikoto")
                .addon_manifest()
                .clone(),
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
        let _packages = super::super::tests::provider_package_fixture_guard();
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
        addon.manifest = super::super::tests::provider_package_fixture("anikoto")
            .addon_manifest()
            .clone();
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
    fn specials_keep_public_ids_when_metadata_belongs_to_another_addon() {
        for id in [
            "tt0994314:0:1",
            "tmdb:31724:0:1",
            "mal:1575:0:1",
            "anilist:1575:0:1",
        ] {
            assert!(public_stream_id(id), "{id}");
        }
        for id in [
            "tt0994314:0:0",
            "tt0994314:-1:1",
            "tt0994314:0:-1",
            "tmdb:0:0:1",
            "tmdb:31724:4294967296:1",
            "native:episode:special",
        ] {
            assert!(!public_stream_id(id), "{id}");
        }
        for name in ["Torrentio", "AIOStreams"] {
            let addon = Installed {
                url: format!("https://{}.example/configured", name.to_ascii_lowercase()),
                label: name.into(),
                enabled: true,
                configure_ok: Some(false),
                available: true,
                generation: 1,
                manifest: serde_json::from_value(serde_json::json!({
                    "id":name,"name":name,"version":"1","types":["series"],
                    "resources":[{"name":"stream","types":["series"],"idPrefixes":["tt"]}]
                }))
                .unwrap(),
            };
            let donor = Some("https://metadata.example/addon");
            let canonical = "tt0994314:0:1";
            assert_eq!(
                stream_endpoint_owned(&addon, "series", canonical, None, &[], donor),
                Some(format!("{}/stream/series/{canonical}.json", addon.url))
            );
            // A confirmed public special can also resolve a private donor ID;
            // an unresolved private ID must still never leave its owner.
            assert_eq!(
                stream_endpoint_owned(
                    &addon,
                    "series",
                    "native:episode:special",
                    None,
                    &[canonical.into()],
                    donor
                ),
                Some(format!("{}/stream/series/{canonical}.json", addon.url))
            );
            assert!(
                stream_endpoint_owned(&addon, "series", "native:episode:special", None, &[], donor)
                    .is_none()
            );
        }
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
        // Native providers can supply context to other contextual sources too.
        modal.videos.last_mut().unwrap().season = Some(1);
        assert!(source_stream_lookup(&modal, &modal.request_id).is_some());
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

fn sort_streams_by_owner(rows: &mut [StreamUi], owner_order: &[String]) {
    rows.sort_by_key(|row| {
        owner_order
            .iter()
            .position(|owner| owner == &row.owner_url)
            .unwrap_or(usize::MAX)
    });
}

/// Downloads remain pinned first. Promote only the first matching remote row;
/// all other rows retain installed-owner order and each owner's own ranking.
fn promote_last_stream(rows: &mut Vec<StreamUi>, identity: Option<&str>) {
    let Some(identity) = identity else { return };
    let Some(index) = rows
        .iter()
        .position(|row| row.download.is_none() && row.choice_id.as_deref() == Some(identity))
    else {
        return;
    };
    let first_remote = rows
        .iter()
        .position(|row| row.download.is_none())
        .unwrap_or(index);
    if index != first_remote {
        let row = rows.remove(index);
        rows.insert(first_remote, row);
    }
}

fn refresh_stream_owner_order(state: &mut Shared) {
    let selected = state
        .stream_filter
        .checked_sub(1)
        .and_then(|i| state.stream_addons.get(i))
        .cloned();
    let installed: HashMap<String, usize> = state
        .installed
        .iter()
        .enumerate()
        .map(|(index, addon)| (addon.url.clone(), index))
        .collect();
    state
        .stream_owner_order
        .sort_by_key(|owner| installed.get(owner).copied().unwrap_or(usize::MAX));
    state.stream_addons.sort_by_key(|label| {
        state
            .stream_owner_order
            .iter()
            .position(|owner| state.stream_owner_labels.get(owner) == Some(label))
            .unwrap_or(usize::MAX)
    });
    state.stream_filter = selected
        .as_ref()
        .and_then(|selected| {
            state
                .stream_addons
                .iter()
                .position(|label| label == selected)
        })
        .map_or(0, |index| index + 1);
}

/// Full publications replace one transport's rows; labels are presentation,
/// so two addons with the same label must not erase each other's results.
fn replace_stream_snapshot(rows: &mut Vec<StreamUi>, owner: &str, snapshot: Vec<StreamUi>) {
    rows.retain(|row| row.owner_url != owner);
    let mut ids = HashSet::new();
    rows.extend(
        snapshot
            .into_iter()
            .filter(|row| row.owner_url == owner && ids.insert(row.id.clone())),
    );
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

/// Reconcile the bounded, 25-row page without resetting retained delegates.
/// Addon responses preserve existing relative order; inserting their new rows
/// must not restart earlier rows' entrance fades or Android hold timers.
pub(crate) fn update_stream_rows(model: &VecModel<StreamRow>, rows: &[StreamRow]) {
    let same_shape = stream_rows_same_shape(model, rows);
    for (index, row) in rows.iter().enumerate() {
        if !same_shape {
            // Remove vanished rows at this slot before looking for a moved
            // identity. That keeps the remaining delegates intact on shrink.
            while model.row_data(index).is_some_and(|current| {
                current.id != row.id && !rows[index..].iter().any(|next| next.id == current.id)
            }) {
                model.remove(index);
            }
            if model
                .row_data(index)
                .is_none_or(|current| current.id != row.id)
            {
                if let Some(previous) = (index + 1..model.row_count())
                    .find(|&previous| model.row_data(previous).is_some_and(|old| old.id == row.id))
                {
                    model.remove(previous);
                }
                model.insert(index, row.clone());
                continue;
            }
        }
        if model.row_data(index).as_ref() != Some(row) {
            model.set_row_data(index, row.clone());
        }
    }
    while model.row_count() > rows.len() {
        model.remove(rows.len());
    }
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
    #[cfg(test)]
    EPISODE_READ_TRACE.with(|trace| {
        if let Some(reads) = trace.borrow_mut().as_mut() {
            reads.push((type_.to_owned(), id.to_owned()));
        }
    });
    read_json::<Vec<Video>>(&episodes_key(type_, id))
}

#[cfg(test)]
thread_local! {
    static EPISODE_READ_TRACE: std::cell::RefCell<Option<Vec<(String, String)>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn trace_episode_reads(action: impl FnOnce()) -> Vec<(String, String)> {
    EPISODE_READ_TRACE.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
    action();
    EPISODE_READ_TRACE.with(|trace| trace.borrow_mut().take().unwrap())
}
fn retain_episode_artwork(videos: &mut [Video], cached: &[Video]) {
    let cached = cached
        .iter()
        .filter(|video| !video.id.is_empty())
        .map(|video| (video.id.as_str(), video))
        .collect::<std::collections::HashMap<_, _>>();
    for video in videos {
        if video
            .thumbnail
            .as_ref()
            .is_none_or(|url| url.trim().is_empty())
            && let Some(previous) = cached.get(video.id.as_str())
        {
            // Native IDs are the stable identity; season numbers may have
            // changed through enrichment. Never transfer artwork by position.
            video.thumbnail = previous.thumbnail.clone();
        }
    }
}
/// Save and return the same reconciled list consumed by Detail, prefetch and
/// Home. The stable cache key supplies the baseline across provider refreshes;
/// the complete parent response is required to prove cross-ID episode matches.
static EPISODE_RECONCILE: Mutex<()> = Mutex::new(());

pub(crate) fn write_episode_meta_cache_for(type_: &str, id: &str, item: &MetaItem) -> Vec<Video> {
    write_episode_meta_cache_guarded(type_, id, item, || true).unwrap_or_default()
}

pub(super) fn write_episode_meta_cache_guarded(
    type_: &str,
    id: &str,
    item: &MetaItem,
    current: impl FnOnce() -> bool,
) -> Option<Vec<Video>> {
    if !meta_matches_request(item, type_, id) {
        return None;
    }
    // Detail, Home and prefetch may finish on different workers. Their
    // read/reconcile/write must be one transaction so a sparse late reply
    // cannot overwrite coverage discovered by a concurrent full response.
    let _guard = EPISODE_RECONCILE.lock().unwrap();
    if !current() {
        return None;
    }
    let mut item = item.clone();
    item.videos.retain(|v| v.season.is_some());
    let known = read_episodes_cache_for(type_, id).unwrap_or_default();
    let mut videos = nova_providers::reconcile_episode_metadata(&item, &known);
    release_reminders::annotate_season_completion(&item, &mut videos);
    Some(write_episodes_cache_for(type_, id, &videos))
}

pub(crate) fn write_episodes_cache_for(type_: &str, id: &str, videos: &[Video]) -> Vec<Video> {
    let mut videos = videos.to_vec();
    if let Some(cached) = read_episodes_cache_for(type_, id) {
        retain_episode_artwork(&mut videos, &cached);
    }
    write_json(&episodes_key(type_, id), &videos);
    progress_projection::metadata_changed();
    smart_offline::trigger();
    videos
}
pub(crate) fn meta_header_key(type_: &str, id: &str) -> String {
    format!("meta_header:{type_}\u{1}{id}")
}
pub(crate) fn read_meta_header_for(type_: &str, id: &str) -> Option<MetaHeader> {
    read_json::<MetaHeader>(&meta_header_key(type_, id))
}
pub(crate) fn write_meta_header_for(type_: &str, id: &str, header: &MetaHeader) {
    write_json(&meta_header_key(type_, id), header);
    if let Some(bridge) = super::bridge::with_global_bridge(Clone::clone) {
        bridge.smart_collection_metadata_changed();
    }
}
/// Build a header snapshot from a fetched meta item (empty when the addon
/// sent nothing usable).
pub(crate) fn meta_header_from_item(item: &MetaItem) -> MetaHeader {
    let mut preview = item.preview.clone();
    preview.extra.extend(item.extra.clone());
    tracking::remember_source(&preview);
    MetaHeader {
        details: detail_extras::DetailExtras::from_item(item),
        // Persist only decoded poster URLs (published by the image pipeline).
        // A metadata URL alone may still return 404 or fail to decode.
        poster_url: String::new(),
        background_url: item.preview.background.clone().unwrap_or_default(),
        logo_url: item.preview.logo.clone().unwrap_or_default(),
        description: item.preview.description.clone().unwrap_or_default(),
        genres: item.preview.genres.clone(),
        year: item.preview.year_str().unwrap_or_default(),
        season_backdrops: item.season_backdrops(),
    }
}

/// An HTTP success is not enough: a buggy endpoint may return a different
/// title or media type. Never cache/apply that response under the requested ID.
pub(crate) fn meta_matches_request(item: &MetaItem, type_: &str, id: &str) -> bool {
    item.preview.id == id && item.preview.type_ == type_
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
/// text slots and updating supplied season-art URLs. Returns true when the
/// stored value changed (or was created with non-empty content).
pub(crate) fn merge_meta_header_for(type_: &str, id: &str, fresh: &MetaHeader) -> bool {
    if fresh.poster_url.is_empty()
        && fresh.background_url.is_empty()
        && fresh.logo_url.is_empty()
        && fresh.description.is_empty()
        && fresh.genres.is_empty()
        && fresh.year.is_empty()
        && fresh.season_backdrops.is_empty()
        && fresh.details == Default::default()
    {
        return false;
    }
    let mut cached = read_meta_header_for(type_, id).unwrap_or_default();
    let mut touched = false;
    touched |= cached.details.merge(&fresh.details);
    for (&season, url) in &fresh.season_backdrops {
        // A newer season image replaces an older cached URL even when text
        // stays gap-filled for a finished show.
        if cached.season_backdrops.get(&season) != Some(url) {
            cached.season_backdrops.insert(season, url.clone());
            touched = true;
        }
    }
    if !fresh.poster_url.trim().is_empty() && cached.poster_url != fresh.poster_url {
        cached.poster_url = fresh.poster_url.clone();
        touched = true;
    }
    if !fresh.background_url.trim().is_empty() && cached.background_url != fresh.background_url {
        cached.background_url = fresh.background_url.clone();
        touched = true;
    }
    if !fresh.logo_url.trim().is_empty() && cached.logo_url != fresh.logo_url {
        cached.logo_url = fresh.logo_url.clone();
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

    #[test]
    fn refresh_gate_rejects_busy_and_repeated_actions_until_the_deadline() {
        let start = Instant::now();
        let mut gate = StreamRefreshGate::default();
        assert!(!gate.claim(start, true));
        assert!(gate.ready(start));
        assert!(gate.claim(start, false));
        assert!(!gate.claim(
            start + STREAM_REFRESH_COOLDOWN - Duration::from_nanos(1),
            false
        ));
        let deadline = start + STREAM_REFRESH_COOLDOWN;
        assert!(
            !gate.claim(deadline, true),
            "a completed cooldown cannot bypass active fetching"
        );
        assert!(gate.ready(deadline));
        assert!(
            gate.claim(deadline, false),
            "the exact deadline is eligible"
        );
        assert!(
            !gate.claim(deadline, false),
            "a keyboard repeat cannot claim twice"
        );
        assert!(!gate.ready(deadline + STREAM_REFRESH_COOLDOWN - Duration::from_nanos(1)));
        assert!(gate.ready(deadline + STREAM_REFRESH_COOLDOWN));
    }

    #[test]
    fn refresh_gate_rearms_early_timers_and_rejects_old_activation_timers() {
        let start = Instant::now();
        let mut gate = StreamRefreshGate::default();
        assert!(gate.claim(start, false));
        let (ready, timer) = gate.publication(start, 10);
        assert!(!ready);
        let (wait, first) = timer.unwrap();
        assert_eq!(wait, STREAM_REFRESH_COOLDOWN);
        assert!(gate.publication(start, 10).1.is_none());
        assert!(gate.wake(first));
        let early = start + STREAM_REFRESH_COOLDOWN - Duration::from_millis(1);
        let (ready, timer) = gate.publication(early, 10);
        assert!(!ready);
        let (wait, rearmed) = timer.unwrap();
        assert_eq!(wait, Duration::from_millis(1));
        assert!(
            !gate.wake(first),
            "an older early wake cannot clear the current timer"
        );
        // Returning through a new activation must schedule its own callback;
        // the original activation's callback is no longer authorized to run.
        let (_, timer) = gate.publication(early, 11);
        let (_, replacement) = timer.unwrap();
        assert!(!gate.wake(rearmed));
        assert!(gate.wake(replacement));
        assert_eq!(
            gate.publication(start + STREAM_REFRESH_COOLDOWN, 11),
            (true, None)
        );
    }

    #[test]
    fn detail_entry_uses_saved_metadata_and_resolves_its_action_before_refresh() {
        // Opening reads persistent caches and uses Slint's global backend.
        // Keep both isolated from other tests and the user's saved library.
        const CHILD: &str = "NOVA_DETAIL_ENTRY_TEST_ROOT";
        let Some(root) = std::env::var_os(CHILD) else {
            let root = std::env::temp_dir().join(format!(
                "nova-detail-entry-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(CHILD, &root)
                .env("XDG_CACHE_HOME", root.join("cache"))
                .args([
                    "--exact",
                    "app::detail::tests::detail_entry_uses_saved_metadata_and_resolves_its_action_before_refresh",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            let _ = fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        storage::init_at(&root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = AppWindow::new().unwrap();
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
        let preview: MetaPreview = serde_json::from_value(serde_json::json!({
            "id": "entry", "type": "series", "name": "Featured entry",
            "releaseInfo": "2020", "genres": ["Catalog genre"],
            "description": "Short catalog synopsis"
        }))
        .unwrap();
        let mut item: MetaItem = serde_json::from_value(serde_json::json!({
            "id": "entry", "type": "series", "name": "Featured entry",
            "releaseInfo": "2020–2026", "genres": ["Drama", "Comedy"],
            "description": "Full detail synopsis", "status": "Continuing",
            "videos": [{"id":"entry:1:1", "title":"Future episode", "season":1,
                "episode":1, "released":"2999-01-01T00:00:00Z"}]
        }))
        .unwrap();
        write_meta_header_for("series", "entry", &meta_header_from_item(&item));
        write_episodes_cache_for("series", "entry", &item.videos);
        bridge.shared.lock().unwrap().home_showcase_displayed = Some(preview.clone());
        let close = || {
            app.set_modal_visible(false);
            bridge.modal_closed();
        };
        // Repeated Home opens must use the last resolved header, rather than
        // replaying a partial catalog header until another response arrives.
        for _ in 0..3 {
            bridge.home_showcase_picked();
            assert!(app.get_modal_visible());
            assert_eq!(app.get_selected_year(), "2020–2026");
            assert_eq!(app.get_selected_description(), "Full detail synopsis");
            assert_eq!(
                app.get_selected_genre_list().iter().collect::<Vec<_>>(),
                [SharedString::from("Drama"), SharedString::from("Comedy")]
            );
            assert_eq!(app.get_detail_watch_label(), text::tr("Choose an episode"));
            let genres = app.get_selected_genre_list();
            let token = bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .open_token
                .clone();
            bridge.apply_episode_meta("entry".into(), Some(item.clone()), true, token);
            assert_eq!(app.get_detail_watch_label(), text::tr("Choose an episode"));
            assert_eq!(
                app.get_selected_genre_list(),
                genres,
                "an unchanged response must retain the existing genre delegates"
            );
            close();
        }
        // An available cached episode already has its precise action on entry.
        item.videos[0].released = Some("2020-01-01T00:00:00Z".into());
        write_episodes_cache_for("series", "entry", &item.videos);
        bridge.home_showcase_picked();
        assert_eq!(
            app.get_detail_watch_label(),
            text::watch_action("S1 E1", false)
        );
        close();
        bridge.home_showcase_watch_now();
        assert!(app.get_modal_visible());
        assert!(app.get_stream_selector_open());
        assert!(app.get_detail_deep_stream());
        assert!(!app.get_episode_context().is_empty());
        close();
        // Ordinary opening resumes only the latest watched season. A deep
        // episode must not select its page/card or request a scroll, and newer
        // empty/unrelated history must not displace real viewing progress.
        let resume_preview = MetaPreview {
            id: "resume-series".into(),
            ..preview.clone()
        };
        let mut resume_item = MetaItem {
            preview: resume_preview.clone(),
            status: Some("Continuing".into()),
            videos: (1..=60)
                .map(|episode| Video {
                    id: format!("resume-series:2:{episode}"),
                    season: Some(2),
                    episode: Some(episode),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        for season in [1, 3] {
            resume_item.videos.push(Video {
                id: format!("resume-series:{season}:1"),
                season: Some(season),
                episode: Some(1),
                ..Default::default()
            });
        }
        {
            let mut state = bridge.shared.lock().unwrap();
            for (series, episode, timestamp, watched) in [
                ("resume-series", "resume-series:1:1", 100, true),
                ("resume-series", "resume-series:2:57", 200, true),
                ("resume-series", "resume-series:3:1", 300, false),
                ("resume-series", "missing-episode", 400, true),
                ("other-series", "resume-series:3:1", 500, true),
            ] {
                state.progress.insert(
                    progress_map_key(series, episode),
                    EpisodeProgress {
                        series_id: series.into(),
                        episode_id: episode.into(),
                        updated_at_secs: timestamp,
                        watched,
                        ..Default::default()
                    },
                );
            }
        }
        let assert_season_only = |season_index| {
            assert_eq!(app.get_season_combo_idx(), season_index);
            assert_eq!(app.get_episode_page(), 0);
            assert_eq!(app.get_detail_kb_ep(), 0);
            assert_eq!(app.get_detail_episode_reveal_request(), 0);
            assert!(!app.get_stream_selector_open());
        };
        write_episodes_cache_for("series", "resume-series", &resume_item.videos);
        bridge.open_preview(resume_preview.clone(), 0, false, None, false);
        assert_season_only(1);
        bridge.season_picked(0);
        let token = bridge
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .unwrap()
            .open_token
            .clone();
        bridge.apply_episode_meta(
            "resume-series".into(),
            Some(resume_item.clone()),
            true,
            token,
        );
        assert_season_only(0); // a background response preserves a manual choice
        close();
        bridge.open_preview(resume_preview.clone(), 0, false, None, false);
        assert_season_only(1); // newer history takes priority over the saved season
        // Specials sort last. Removing and then rediscovering an earlier
        // numbered season must retain the selected season's identity.
        let mut changed_inventory = resume_item.clone();
        changed_inventory.videos.push(Video {
            id: "resume-series:0:1".into(),
            season: Some(0),
            episode: Some(1),
            ..Default::default()
        });
        let token = bridge
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .unwrap()
            .open_token
            .clone();
        bridge.apply_episode_meta(
            "resume-series".into(),
            Some(changed_inventory.clone()),
            true,
            token.clone(),
        );
        assert_season_only(1);
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .seasons,
            [1, 2, 3, 0]
        );
        changed_inventory
            .videos
            .retain(|video| video.season.is_some_and(|season| season >= 2));
        bridge.apply_episode_meta(
            "resume-series".into(),
            Some(changed_inventory.clone()),
            true,
            token.clone(),
        );
        assert_season_only(0);
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .seasons[0],
            2
        );
        changed_inventory.videos.push(Video {
            id: "resume-series:1:1".into(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        });
        bridge.apply_episode_meta("resume-series".into(), Some(changed_inventory), true, token);
        assert_season_only(1);
        close();
        // Recency, not the highest season number, also handles a rewatch.
        {
            let mut state = bridge.shared.lock().unwrap();
            let progress = state
                .progress
                .get_mut(&progress_map_key("resume-series", "resume-series:1:1"))
                .unwrap();
            progress.watched = false;
            progress.position_secs = 30.0;
            progress.updated_at_secs = 600;
        }
        bridge.open_preview(resume_preview.clone(), 0, false, None, false);
        assert_season_only(0);
        bridge.season_picked(2);
        close();
        bridge
            .shared
            .lock()
            .unwrap()
            .progress
            .retain(|_, progress| progress.series_id != "resume-series");
        bridge.open_preview(resume_preview.clone(), 0, false, None, false);
        assert_season_only(2); // without history, keep a valid saved season
        close();
        bridge
            .shared
            .lock()
            .unwrap()
            .detail_snapshots
            .remove("resume-series");
        bridge.open_preview(resume_preview.clone(), 0, false, None, false);
        assert_season_only(0);
        close();
        // A cold first population follows the same rule when metadata arrives.
        resume_item.preview.id = "cold-resume".into();
        bridge.shared.lock().unwrap().progress.insert(
            progress_map_key("cold-resume", "resume-series:2:57"),
            EpisodeProgress {
                series_id: "cold-resume".into(),
                episode_id: "resume-series:2:57".into(),
                play_count: 1,
                updated_at_secs: 700,
                ..Default::default()
            },
        );
        bridge.open_preview(resume_item.preview.clone(), 0, false, None, false);
        let token = bridge
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .unwrap()
            .open_token
            .clone();
        bridge.apply_episode_meta("cold-resume".into(), Some(resume_item), false, token);
        assert_season_only(1);
        close();
        // A first visit has no target yet, so it offers episode selection
        // while loading, rather than advertising a speculative Start action.
        let uncached = MetaPreview {
            id: "uncached".into(),
            ..preview.clone()
        };
        write_meta_header_for("series", "uncached", &MetaHeader::default());
        bridge.open_preview(uncached, 0, false, None, false);
        assert_eq!(app.get_detail_watch_label(), text::tr("Choose an episode"));
        // Empty cached text still preserves usable catalog fields.
        assert_eq!(app.get_selected_year(), "2020");
        assert_eq!(app.get_selected_description(), "Short catalog synopsis");
        let token = bridge
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .unwrap()
            .open_token
            .clone();
        bridge.apply_episode_meta("uncached".into(), None, false, token);
        assert_eq!(
            app.get_detail_watch_label(),
            text::tr("Start"),
            "a proven seasonless fallback keeps direct streams"
        );
        close();
        // Library entry uses the same resolved header despite a stale saved
        // preview; direct movie actions must also clear a previous series label.
        let entry: LibraryEntry = serde_json::from_value(serde_json::json!({
            "id":"entry", "type_":"series", "name":"Featured entry", "year":"2020",
            "poster_url":"", "genres":["Catalog genre"], "description":"Short synopsis",
            "categories":[], "watch_status":"Auto"
        }))
        .unwrap();
        bridge.open_library_entry(entry);
        assert_eq!(app.get_selected_year(), "2020–2026");
        assert_eq!(app.get_selected_description(), "Full detail synopsis");
        assert_eq!(
            app.get_detail_watch_label(),
            text::watch_action("S1 E1", false)
        );
        // Actual metadata changes are still published and saved for reentry.
        item.preview.genres.push("Mystery".into());
        item.preview.description = Some("Updated full synopsis".into());
        let token = bridge
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .unwrap()
            .open_token
            .clone();
        bridge.apply_episode_meta("entry".into(), Some(item), true, token);
        assert_eq!(app.get_selected_genre_list().row_count(), 3);
        assert_eq!(app.get_selected_description(), "Updated full synopsis");
        close();
        bridge.home_showcase_picked();
        assert_eq!(app.get_selected_genre_list().row_count(), 3);
        assert_eq!(app.get_selected_description(), "Updated full synopsis");
        close();
        bridge.open_preview(
            MetaPreview {
                id: "movie".into(),
                type_: "movie".into(),
                extra: HashMap::from([("_malId".into(), serde_json::json!(123))]),
                ..preview
            },
            0,
            false,
            None,
            false,
        );
        assert_eq!(app.get_detail_watch_label(), text::tr("Start"));
        // Related-title Back is a nested detail return, not a catalog close.
        // Mount the real host before replacing its viewport callbacks below.
        app.show().unwrap();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
        let source_ids = tracking::current_source_ids("movie", "movie");
        let content = (*app).global::<nova_ui_shared::DetailContent>();
        let offset = Rc::new(std::cell::Cell::new(-840.0_f32));
        let value = offset.clone();
        content.on_scroll_y(move || value.get());
        let value = offset.clone();
        content.on_restore_scroll_y(move |y| value.set(y));
        content.set_synopsis_expanded(true);
        content.set_trailers_expanded(true);
        let related = MetaPreview {
            id: "child".into(),
            type_: "series".into(),
            name: "Child".into(),
            ..Default::default()
        };
        bridge
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_mut()
            .unwrap()
            .details
            .related = vec![related];
        bridge.detail_extra_picked(2, &serde_json::to_string(&("series", "child")).unwrap());
        assert_eq!(app.get_selected_title(), "Child");
        assert_eq!(app.get_detail_tab(), 0);
        assert!(!content.get_synopsis_expanded());
        offset.set(-420.0);
        let related = MetaPreview {
            id: "grandchild".into(),
            type_: "movie".into(),
            name: "Grandchild".into(),
            ..Default::default()
        };
        bridge
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_mut()
            .unwrap()
            .details
            .related = vec![related];
        bridge.detail_extra_picked(2, &serde_json::to_string(&("movie", "grandchild")).unwrap());
        assert_eq!(app.get_selected_title(), "Grandchild");
        assert!(bridge.related_detail_back());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
        assert_eq!(app.get_selected_title(), "Child");
        assert_eq!(offset.get(), -420.0);
        assert!(bridge.related_detail_back());
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
        assert_eq!(offset.get(), -840.0);
        assert_eq!(app.get_selected_title(), "Featured entry");
        assert_eq!(tracking::current_source_ids("movie", "movie"), source_ids);
        assert!(content.get_synopsis_expanded());
        assert!(content.get_trailers_expanded());
        assert!(app.get_modal_visible());
        assert!(!bridge.related_detail_back());
        // Ordinary catalog opens cannot inherit a related-title stack.
        bridge.detail_extra_picked(2, &serde_json::to_string(&("series", "child")).unwrap());
        assert!(bridge.related_detail_back());
        bridge.detail_extra_picked(2, &serde_json::to_string(&("series", "child")).unwrap());
        assert_eq!(
            bridge.shared.lock().unwrap().related_detail_history.len(),
            1
        );
        bridge.open_preview(
            MetaPreview {
                id: "unrelated".into(),
                type_: "movie".into(),
                name: "Unrelated".into(),
                ..Default::default()
            },
            0,
            false,
            None,
            false,
        );
        offset.set(-77.0);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
        assert_eq!(
            offset.get(),
            -77.0,
            "a delayed restoration must not scroll a different title"
        );
        assert!(!bridge.related_detail_back());
    }

    #[test]
    fn live_owner_order_preserves_the_selected_addon_pill() {
        let installed = |url: &str, label: &str| Installed {
            url: url.into(),
            label: label.into(),
            enabled: true,
            configure_ok: None,
            manifest: serde_json::from_value(serde_json::json!({
                "id": url, "name": label, "version": "1.0.0",
                "resources": ["stream"], "types": ["movie"], "catalogs": []
            }))
            .unwrap(),
            available: true,
            generation: 0,
        };
        let mut state = Shared {
            installed: vec![installed("b", "Beta"), installed("a", "Alpha")],
            stream_owner_order: vec!["a".into(), "b".into()],
            stream_owner_labels: HashMap::from([
                ("a".into(), "Alpha".into()),
                ("b".into(), "Beta".into()),
            ]),
            stream_addons: vec!["Alpha".into(), "Beta".into()],
            stream_filter: 1,
            ..Default::default()
        };
        refresh_stream_owner_order(&mut state);
        assert_eq!(state.stream_owner_order, ["b", "a"]);
        assert_eq!(state.stream_addons, ["Beta", "Alpha"]);
        assert_eq!(state.stream_filter, 2, "Alpha stays selected after moving");
        state.installed.swap(0, 1);
        refresh_stream_owner_order(&mut state);
        assert_eq!(state.stream_addons, ["Alpha", "Beta"]);
        assert_eq!(state.stream_filter, 1);
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn stream_selector_returns_to_its_origin_and_movies_keep_a_watch_action() {
        // Storage and Slint are process-global; isolate this navigation test
        // so its progress fixtures never touch the user's library.
        const ROOT: &str = "NOVA_STREAM_SELECTOR_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-stream-selector-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .env("XDG_CACHE_HOME", root.join("cache"))
                .args([
                    "--exact",
                    "app::detail::tests::stream_selector_returns_to_its_origin_and_movies_keep_a_watch_action",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            let _ = fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        storage::init_at(&root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let (hi, _) = mpsc::channel();
        let (lo, _) = mpsc::channel();
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        bridge.shared.lock().unwrap().modal_item = Some(ModalItem {
            details: Default::default(),
            open_token: Arc::new(()),
            pending_watch_now: None,
            episodes_loading: false,
            id: "movie".into(),
            type_: "movie".into(),
            request_id: "movie".into(),
            videos: vec![],
            season_backdrops: HashMap::new(),
            seasons: vec![],
            season_index: 0,
            episode_page: 0,
            name: "Movie".into(),
            year: "2016".into(),
            poster_url: String::new(),
            background_url: String::new(),
            logo_url: String::new(),
            description: String::new(),
            genres: vec![],
        });
        app.set_modal_visible(true);
        app.set_detail_is_movie(true);
        app.set_kb_active(true);
        // Cumulative publications keep their pending spinner until final,
        // preserve focused identities through ranked insertion/reordering,
        // and isolate addons that happen to share a display label.
        let context = StreamResponseContext {
            request_id: "movie".into(),
            media_type: "movie".into(),
            generation: 1,
            addon: "Same label".into(),
            owner_url: "owner-a".into(),
            owner_generation: 0,
            settings_generation: 0,
            tracks_settings: false,
            cache_hit: false,
        };
        let owned = |owner: &str, id: &str| {
            let mut row = row(owner, id);
            row.addon = "Same label".into();
            row
        };
        {
            let mut state = bridge.shared.lock().unwrap();
            state.stream_generation = 1;
            state.stream_addons = vec!["Same label".into()];
            state.stream_pending = vec!["owner-a".into(), "owner-b".into()];
            state.stream_owner_order = state.stream_pending.clone();
            state.stream_owner_labels = HashMap::from([
                ("owner-a".into(), "Same label".into()),
                ("owner-b".into(), "Same label".into()),
            ]);
        }
        let mut early = context.clone();
        early.owner_url = "owner-b".into();
        early.cache_hit = true;
        bridge.stream_addon_updated(early, vec![owned("owner-b", "other")], true);
        assert_eq!(
            app.get_streams().row_count(),
            1,
            "a cache hit displays before the slower first addon answers"
        );
        assert_eq!(app.get_streams().row_data(0).unwrap().id, "other");
        assert!(app.get_streams_searching());
        bridge.stream_addon_updated(
            context.clone(),
            vec![owned("owner-a", "a"), owned("owner-a", "b")],
            false,
        );
        assert!(app.get_streams_searching());
        assert!(app.get_stream_loading().row_data(0).unwrap());
        assert!(app.get_streams().row_data(0).unwrap().reveal_new);
        app.set_detail_kb_s(0);
        bridge.stream_addon_updated(
            context.clone(),
            vec![
                owned("owner-a", "new"),
                owned("owner-a", "b"),
                owned("owner-a", "a"),
            ],
            false,
        );
        assert_eq!(app.get_detail_kb_s(), 2);
        assert_eq!(app.get_streams().row_data(2).unwrap().id, "a");
        assert!(!app.get_streams().row_data(2).unwrap().reveal_new);
        assert!(app.get_streams().row_data(0).unwrap().reveal_new);
        let mut other_context = context.clone();
        other_context.owner_url = "owner-b".into();
        bridge.stream_addon_updated(other_context, vec![owned("owner-b", "other")], true);
        assert!(app.get_streams_searching());
        bridge.stream_addon_updated(
            context.clone(),
            vec![
                owned("owner-a", "a"),
                owned("owner-a", "new"),
                owned("owner-a", "b"),
            ],
            true,
        );
        assert!(!app.get_streams_searching());
        assert!(!app.get_stream_loading().row_data(0).unwrap());
        assert_eq!(app.get_detail_kb_s(), 0);
        assert_eq!(
            app.get_streams()
                .iter()
                .map(|row| row.id.to_string())
                .collect::<Vec<_>>(),
            ["a", "new", "b", "other"]
        );
        assert_eq!(bridge.shared.lock().unwrap().stream_all.len(), 4);
        let mut cached_context = context.clone();
        cached_context.owner_url = "owner-b".into();
        cached_context.cache_hit = true;
        bridge.stream_addon_updated(
            cached_context.clone(),
            vec![owned("owner-b", "other")],
            true,
        );
        assert_eq!(
            app.get_streams()
                .iter()
                .map(|row| row.id.to_string())
                .collect::<Vec<_>>(),
            ["a", "new", "b", "other"],
            "cached owners retain configured order even with matching addon labels"
        );
        assert_eq!(
            app.get_detail_kb_s(),
            0,
            "cache hits preserve focused identity without promoting an owner"
        );
        bridge.shared.lock().unwrap().stream_filter = 1;
        bridge.apply_stream_filter();
        assert_eq!(
            app.get_streams()
                .iter()
                .map(|row| row.id.to_string())
                .collect::<Vec<_>>(),
            ["a", "new", "b", "other"],
            "selecting an addon preserves its original owner order"
        );
        bridge.shared.lock().unwrap().stream_filter = 0;
        // An installed-order change immediately affects the retained request,
        // without refetching or discarding the focused stream identity.
        let installed = |url: &str| {
            Installed {
            url: url.into(), label: "Same label".into(), enabled: true, configure_ok: None,
            manifest: serde_json::from_value(serde_json::json!({ "id": url, "name": "Same label", "version": "1.0.0", "resources": ["stream"], "types": ["movie"], "catalogs": [] })).unwrap(),
            available: true, generation: 0,
        }
        };
        bridge.shared.lock().unwrap().installed = vec![installed("owner-b"), installed("owner-a")];
        app.set_detail_kb_s(0);
        bridge.apply_stream_filter();
        assert_eq!(
            app.get_streams()
                .iter()
                .map(|row| row.id.to_string())
                .collect::<Vec<_>>(),
            ["other", "a", "new", "b"]
        );
        assert_eq!(app.get_detail_kb_s(), 1);
        bridge.shared.lock().unwrap().installed.swap(0, 1);
        bridge.apply_stream_filter();
        assert_eq!(app.get_streams().row_data(0).unwrap().id, "a");
        bridge.shared.lock().unwrap().installed.clear();
        cached_context.cache_hit = false;
        bridge.stream_addon_updated(cached_context, vec![owned("owner-b", "other")], true);
        app.set_detail_kb_s(2);
        app.set_stream_action_id("b".into());
        app.set_stream_action_open(true);
        bridge.stream_addon_updated(context.clone(), vec![], true);
        assert_eq!(
            app.get_detail_kb_s(),
            0,
            "removed focus clamps to the remaining owner row"
        );
        assert_eq!(app.get_streams().row_data(0).unwrap().id, "other");
        assert!(
            !app.get_stream_action_open(),
            "removed stream actions must close"
        );
        bridge.clear_streams();
        let mut stale_context = context;
        stale_context.cache_hit = true;
        bridge.stream_addon_updated(stale_context, vec![owned("owner-a", "late")], true);
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .stream_cached_owners
                .is_empty()
        );
        assert_eq!(
            app.get_streams().row_count(),
            0,
            "old generations cannot repopulate a closed search"
        );
        // Touch users have no focused stream. A slow, high-priority addon
        // inserting more than one page must not drag them to the old first
        // row's new page through an invisible keyboard cursor.
        app.set_kb_active(false);
        let generation = {
            let mut state = bridge.shared.lock().unwrap();
            state.installed = vec![installed("supernova"), installed("fast")];
            state.stream_owner_order = vec!["supernova".into(), "fast".into()];
            state.stream_owner_labels = HashMap::from([
                ("supernova".into(), "Supernova".into()),
                ("fast".into(), "Fast addon".into()),
            ]);
            state.stream_addons = vec!["Supernova".into(), "Fast addon".into()];
            state.stream_pending = state.stream_owner_order.clone();
            state.stream_page = 0;
            state.stream_generation
        };
        let response = |owner: &str, label: &str| StreamResponseContext {
            request_id: "movie".into(),
            media_type: "movie".into(),
            generation,
            addon: label.into(),
            owner_url: owner.into(),
            owner_generation: 0,
            settings_generation: 0,
            tracks_settings: false,
            cache_hit: false,
        };
        let batch = |owner: &str, label: &str, count: usize| {
            (0..count)
                .map(|i| {
                    let mut row = row(owner, &format!("{owner}-{i}"));
                    row.addon = label.into();
                    row.source = StreamSource::Url(format!("https://example.invalid/{owner}/{i}"));
                    row.choice_id = Some(stream_choice_identity(
                        owner,
                        "movie",
                        "movie",
                        &addons::Stream {
                            url: Some(format!("https://example.invalid/{owner}/{i}")),
                            ..Default::default()
                        },
                    ));
                    row
                })
                .collect()
        };
        bridge.stream_addon_updated(
            response("fast", "Fast addon"),
            batch("fast", "Fast addon", 5),
            true,
        );
        assert_eq!(app.get_streams().row_data(0).unwrap().id, "fast-0");
        bridge.stream_addon_updated(
            response("supernova", "Supernova"),
            batch("supernova", "Supernova", 30),
            false,
        );
        assert_eq!(
            app.get_stream_page(),
            0,
            "touch publication must keep the selected page"
        );
        assert_eq!(app.get_streams().row_data(0).unwrap().id, "supernova-0");
        assert_eq!(app.get_stream_addons().row_data(0).unwrap(), "Supernova");
        // An explicit page choice also remains selected through later batches.
        bridge.stream_page_picked(1);
        bridge.stream_addon_updated(
            response("supernova", "Supernova"),
            batch("supernova", "Supernova", 60),
            true,
        );
        assert_eq!(app.get_stream_page(), 1);
        assert_eq!(app.get_streams().row_data(0).unwrap().id, "supernova-25");
        // Keyboard navigation still follows an actively selected row by ID.
        app.set_kb_active(true);
        app.set_detail_kb_s(25);
        bridge.stream_addon_updated(
            response("supernova", "Supernova"),
            batch("supernova", "Supernova", 10),
            true,
        );
        assert_eq!(app.get_stream_page(), 0);
        assert_eq!(
            app.get_detail_kb_s(),
            14,
            "removed keyboard selection clamps to the last remaining row"
        );
        // One last-used source overrides normal addon priority, while filters,
        // progressive updates and keyboard identity still behave normally.
        let choice = stream_choice_identity(
            "fast",
            "movie",
            "movie",
            &addons::Stream {
                url: Some("https://example.invalid/fast/4".into()),
                ..Default::default()
            },
        );
        // A peer's selection refreshes the open selector without playing it
        // or echoing another selection back into the record owner.
        let owner = nova_sync::local_store().unwrap();
        {
            let mut store = owner.lock().unwrap();
            store.set(
                sync::DOMAIN_LAST_STREAM,
                "selection",
                Some(
                    serde_json::json!({
                        "version": 1, "identity": choice, "selected_at": nova_config::now_secs(),
                    })
                    .to_string(),
                ),
                0,
                2,
            );
            store.save().unwrap();
        }
        let before = owner.lock().unwrap().digest();
        app.set_stream_selector_open(true);
        bridge.sync_apply(vec![sync::DOMAIN_LAST_STREAM.into()]);
        assert_eq!(owner.lock().unwrap().digest(), before);
        assert!(
            app.get_streams()
                .iter()
                .any(|row| row.id == "fast-4" && row.last_used)
        );
        assert!(!app.get_player_open());
        app.set_kb_active(false);
        bridge.stream_page_picked(0);
        assert_eq!(app.get_streams().row_data(0).unwrap().id, "fast-4");
        assert!(app.get_streams().row_data(0).unwrap().last_used);
        assert_eq!(
            app.get_streams().iter().filter(|row| row.last_used).count(),
            1
        );
        app.set_kb_active(true);
        app.set_detail_kb_s(1); // supernova-0, not the promoted row
        bridge.stream_addon_updated(
            response("supernova", "Supernova"),
            batch("supernova", "Supernova", 30),
            true,
        );
        assert_eq!(app.get_detail_kb_s(), 1);
        assert_eq!(app.get_streams().row_data(1).unwrap().id, "supernova-0");
        bridge.stream_filter_picked(1);
        assert!(app.get_streams().iter().all(|row| !row.last_used));
        assert_eq!(app.get_streams().row_data(0).unwrap().id, "supernova-0");
        bridge.stream_filter_picked(2);
        assert_eq!(app.get_streams().row_data(0).unwrap().id, "fast-4");
        bridge.stream_filter_picked(0);
        bridge.stream_addon_updated(
            response("fast", "Fast addon"),
            batch("fast", "Fast addon", 4),
            true,
        );
        assert!(
            app.get_streams().iter().all(|row| !row.last_used),
            "missing choices do not invent a row"
        );
        bridge.stream_addon_updated(
            response("fast", "Fast addon"),
            batch("fast", "Fast addon", 5),
            true,
        );
        let (displayed, rows, _) = bridge.compute_stream_view();
        assert_eq!(
            displayed[0].id, "fast-4",
            "later batches promote the returning choice"
        );
        assert!(rows[0].last_used);
        bridge
            .last_stream
            .remember(choice, nova_config::now_secs() - 7 * 24 * 60 * 60);
        let (displayed, rows, _) = bridge.compute_stream_view();
        assert_eq!(displayed[0].id, "supernova-0");
        assert!(rows.iter().all(|row| !row.last_used));
        bridge.clear_streams();
        bridge.refresh_watch_action();
        assert_eq!(app.get_detail_watch_label(), text::tr("Start"));
        bridge.watch_now();
        assert!(app.get_stream_selector_open());
        bridge.stream_selector_back();
        assert!(!app.get_stream_selector_open());
        assert!(app.get_modal_visible());
        bridge.detail_tab_picked(3);
        assert_eq!(app.get_detail_tab(), 0, "movies cannot enter Episodes");
        bridge.shared.lock().unwrap().progress.insert(
            progress_map_key("movie", "movie"),
            EpisodeProgress {
                series_id: "movie".into(),
                episode_id: "movie".into(),
                position_secs: 60.0,
                duration_secs: 120.0,
                ..Default::default()
            },
        );
        bridge.refresh_watch_action();
        assert_eq!(app.get_detail_watch_label(), text::tr("Continue"));

        {
            let mut state = bridge.shared.lock().unwrap();
            let modal = state.modal_item.as_mut().unwrap();
            modal.id = "series".into();
            modal.type_ = "series".into();
            modal.request_id = "series".into();
        }
        app.set_detail_is_movie(false);
        bridge.refresh_watch_action();
        assert_eq!(app.get_detail_watch_label(), text::tr("Start"));
        bridge.watch_now();
        assert!(
            app.get_stream_selector_open(),
            "seasonless titles keep direct streams"
        );
        bridge.stream_selector_back();
        assert_eq!(app.get_detail_tab(), 0);

        // A featured shortcut made while metadata is loading also resolves
        // into the fallback when no addon can provide an episode list.
        let open_token = {
            let mut state = bridge.shared.lock().unwrap();
            let modal = state.modal_item.as_mut().unwrap();
            modal.episodes_loading = true;
            modal.pending_watch_now = Some(WatchNowOrigin::Featured);
            modal.open_token.clone()
        };
        bridge.apply_episode_meta("series".into(), None, false, open_token);
        assert!(app.get_stream_selector_open());
        assert!(app.get_detail_deep_stream());
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .pending_watch_now
                .is_none()
        );
        app.set_detail_deep_stream(false);
        bridge.stream_selector_back();

        {
            let mut state = bridge.shared.lock().unwrap();
            let modal = state.modal_item.as_mut().unwrap();
            modal.seasons = vec![1];
            modal.videos = vec![Video {
                id: "series:1:1".into(),
                season: Some(1),
                episode: Some(1),
                name: "Pilot".into(),
                released: Some("2020-01-01".into()),
                ..Default::default()
            }];
        }
        app.set_modal_episodes(true);
        bridge.episode_picked(0);
        assert!(app.get_stream_selector_open());
        assert!(!app.get_episode_context().is_empty());
        bridge.stream_selector_back();
        assert!(!app.get_stream_selector_open());
        assert!(app.get_modal_episodes());
        assert!(app.get_episode_context().is_empty());
        // A completed episode is the return anchor, even though Continue
        // would select the next episode. Restore its season and second page.
        {
            let mut state = bridge.shared.lock().unwrap();
            let modal = state.modal_item.as_mut().unwrap();
            modal.seasons = vec![1, 2, 3];
            for season in [2, 3] {
                modal.videos.extend((1..=120).map(|episode| Video {
                    id: format!("series:{season}:{episode}"),
                    season: Some(season),
                    episode: Some(episode),
                    name: format!("Episode {episode}"),
                    ..Default::default()
                }));
            }
            state.progress.insert(
                progress_map_key("series", "series:2:57"),
                EpisodeProgress {
                    series_id: "series".into(),
                    episode_id: "series:2:57".into(),
                    watched: true,
                    updated_at_secs: 100,
                    ..Default::default()
                },
            );
        }
        app.set_episode_filter("Pilot".into());
        let prior_season = app.get_season_combo_idx();
        let prior_page = app.get_episode_page();
        app.set_stream_selector_open(true);
        app.set_episode_context("Pilot".into());
        bridge.stream_selector_back();
        assert_eq!(app.get_season_combo_idx(), prior_season);
        assert_eq!(app.get_episode_page(), prior_page);
        assert_eq!(app.get_episode_filter(), "Pilot");
        assert_eq!(app.get_detail_episode_reveal_request(), 0);
        app.set_stream_selector_open(true);
        app.set_episode_context("Episode 57".into());
        bridge.restore_detail_after_playback();
        assert!(!app.get_stream_selector_open());
        assert_eq!(app.get_season_combo_idx(), 1);
        assert_eq!(app.get_episode_page(), 1);
        assert_eq!(app.get_detail_kb_ep(), 6);
        assert!(app.get_episode_filter().is_empty());
        assert!(app.get_detail_episode_reveal_request() > 0);

        // Mount the real retained shell and exercise its Back callback, not
        // just the backend flag setters. Manual selector cancellation keeps
        // a different season, a filtered second page and its precise offset.
        app.set_animations(false);
        app.set_anim_transitions(false);
        app.show().unwrap();
        let back_bridge = bridge.clone();
        app.on_stream_selector_back(move || back_bridge.stream_selector_back());
        let content = (*app).global::<nova_ui_shared::DetailContent>();
        let settle = || {
            for _ in 0..10 {
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
            }
        };
        for size in [(820, 600), (390, 820)] {
            app.window()
                .set_size(slint::PhysicalSize::new(size.0, size.1));
            bridge.season_picked(2);
            bridge.episode_filter_changed("Episode".into());
            bridge.episode_page_picked(1);
            settle();
            content.invoke_restore_scroll_y(-1300.0);
            settle();
            let offset = content.invoke_scroll_y();
            assert!(offset < -1000.0, "fixture must scroll into the list");
            let first_id = app.get_episode_rows().row_data(0).unwrap().id;
            bridge.episode_picked(56);
            settle();
            assert!(app.get_stream_selector_open());
            content.invoke_dispatch_back();
            settle();
            assert!(!app.get_stream_selector_open());
            assert!(app.get_modal_visible());
            assert!(app.get_modal_episodes());
            assert_eq!(app.get_season_combo_idx(), 2);
            assert_eq!(app.get_episode_page(), 1);
            assert_eq!(app.get_episode_filter(), "Episode");
            assert_eq!(app.get_episode_rows().row_data(0).unwrap().id, first_id);
            assert_eq!(app.get_detail_kb_ep(), 6);
            assert_eq!(app.get_detail_episode_reveal_request(), 0);
            assert!(
                (content.invoke_scroll_y() - offset).abs() < 1.0,
                "selector Back moved the retained episode list from {offset} to {}",
                content.invoke_scroll_y()
            );
        }

        // Keep the following stream-preference fixture on its original season.
        bridge.season_picked(1);
        bridge.episode_filter_changed(SharedString::default());

        // Selection wiring remembers each show independently, then publishes
        // a recommendation for the next episode through the normal row model.
        let series_release = |id: &str, title: &str| {
            let mut stream = row("series-addon", id);
            stream.choice_id = Some(stream_choice_identity(
                "series-addon",
                "anime",
                id,
                &addons::Stream {
                    info_hash: Some("a".repeat(40)),
                    ..Default::default()
                },
            ));
            stream.card.title = title.into();
            // The fixture has no installed resolver: the ordinary selection
            // is remembered before resolution fails without network work.
            stream.source = StreamSource::Resolver {
                provider_url: "nova-provider://missing-test-provider".into(),
                token: String::new(),
                settings_generation: 0,
                addon_generation: 0,
            };
            stream
        };
        let previous = series_release("previous", "Show.S02E01.1080p.WEB-DL.H264-GROUP");
        {
            let mut state = bridge.shared.lock().unwrap();
            let modal = state.modal_item.as_mut().unwrap();
            modal.type_ = "anime".into();
            modal.request_id = "series:2:1".into();
            state.stream_all = vec![previous.clone()];
            state.stream_addons = vec!["series-addon".into()];
            state.stream_filter = 0;
        }
        bridge.apply_stream_filter();
        bridge.stream_picked(0);
        assert!(
            bridge
                .last_stream
                .series_preference(
                    &stream_series_scope("anime", "series").unwrap(),
                    nova_config::now_secs()
                )
                .is_some()
        );
        assert!(app.get_streams().row_data(0).unwrap().last_used);
        bridge.episode_picked(1);
        {
            let mut state = bridge.shared.lock().unwrap();
            state.stream_all = vec![
                series_release("other", "Show.S02E02.720p.HDTV.H265-OTHER"),
                series_release("next", "Show.S02E02.1080p.WEB-DL.H264-GROUP"),
            ];
        }
        bridge.apply_stream_filter();
        let recommended = app.get_streams().row_data(0).unwrap();
        assert_eq!(recommended.id, "next");
        assert!(recommended.recommended);
        assert!(!recommended.last_used);
        assert!(
            !app.get_player_open(),
            "recommendation never starts playback"
        );
        bridge
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_mut()
            .unwrap()
            .type_ = "series".into();
        bridge.apply_stream_filter();
        assert!(
            app.get_streams().iter().all(|row| !row.recommended),
            "same bare ID in another native type must not inherit anime preferences"
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            let modal = state.modal_item.as_mut().unwrap();
            modal.type_ = "anime".into();
            modal.id = "unrelated-series".into();
        }
        bridge.apply_stream_filter();
        assert!(app.get_streams().iter().all(|row| !row.recommended));
        assert_eq!(app.get_streams().row_data(0).unwrap().id, "other");
        {
            let mut state = bridge.shared.lock().unwrap();
            state.modal_item.as_mut().unwrap().id = "series".into();
            state.stream_all.push(previous);
        }
        bridge.apply_stream_filter();
        assert_eq!(app.get_streams().row_data(0).unwrap().id, "previous");
        assert!(app.get_streams().row_data(0).unwrap().last_used);
        assert!(app.get_streams().iter().all(|row| !row.recommended));

        bridge
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_mut()
            .unwrap()
            .type_ = "series".into();

        // Reopening remembers content choices, never a prior navigation
        // position. Late metadata also respects a tab picked while loading.
        app.set_detail_tab(0);
        app.set_episode_filter("Episode".into());
        app.set_detail_kb_zone(3);
        app.set_detail_kb_top(2);
        app.set_detail_kb_ci(1);
        app.set_detail_kb_s(4);
        bridge.save_detail_snapshot();
        bridge.restore_detail_snapshot("series");
        assert_eq!(app.get_detail_tab(), 0);
        assert_eq!(app.get_episode_filter(), "Episode");
        assert_eq!(app.get_detail_kb_zone(), 0);
        assert_eq!(app.get_detail_kb_top(), 0);
        assert_eq!(app.get_detail_kb_ci(), 2);
        assert_eq!(app.get_detail_kb_ep(), 0);
        assert_eq!(app.get_detail_kb_s(), 0);
        let videos = bridge
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .unwrap()
            .videos
            .clone();
        bridge.show_episode_picker("series".into(), videos.clone());
        assert_eq!(app.get_season_combo_idx(), 1);
        assert_eq!(app.get_detail_tab(), 0);
        app.set_detail_tab(3);
        bridge.show_episode_picker("series".into(), videos);
        assert_eq!(
            app.get_detail_tab(),
            3,
            "metadata must not reapply the old tab"
        );
        bridge.restore_detail_snapshot("new-series");
        assert_eq!(
            app.get_detail_tab(),
            3,
            "series choose Episodes before mounting"
        );
        assert!(app.get_episode_filter().is_empty());
        app.set_detail_is_movie(true);
        bridge.restore_detail_snapshot("new-movie");
        assert_eq!(app.get_detail_tab(), 0);
        app.set_detail_is_movie(false);
        app.set_detail_tab(3);
        bridge.refresh_episode_rows();
        bridge.episode_picked(0);
        app.set_detail_deep_stream(true);
        bridge.stream_selector_back();
        assert!(!app.get_modal_visible());
        assert!(!app.get_stream_selector_open());
        assert!(bridge.shared.lock().unwrap().modal_item.is_none());
    }

    #[test]
    fn cached_headers_add_poster_urls_without_breaking_older_storage() {
        let old: MetaHeader =
            serde_json::from_str(r#"{"background_url":"https://images.example/backdrop.jpg"}"#)
                .unwrap();
        assert!(old.poster_url.is_empty());
        assert!(old.logo_url.is_empty());
        let fresh = MetaHeader {
            details: Default::default(),
            poster_url: "https://images.example/poster.jpg".into(),
            logo_url: "https://images.example/title-logo.png".into(),
            ..old
        };
        let restored: MetaHeader =
            serde_json::from_str(&serde_json::to_string(&fresh).unwrap()).unwrap();
        assert_eq!(restored, fresh);
    }

    #[test]
    fn metadata_response_must_match_requested_id_and_type() {
        let item: MetaItem = serde_json::from_value(serde_json::json!({
            "id":"provider:show", "type":"series", "name":"Native title"
        }))
        .unwrap();
        assert!(meta_matches_request(&item, "series", "provider:show"));
        assert!(!meta_matches_request(&item, "series", "provider:other"));
        assert!(!meta_matches_request(&item, "movie", "provider:show"));
    }

    fn row(addon: &str, id: &str) -> StreamUi {
        StreamUi {
            id: id.into(),
            choice_id: None,
            owner_url: addon.into(),
            display: id.into(),
            card: StreamCardText::default(),
            source: StreamSource::Unsupported,
            addon: addon.into(),
            download: None,
        }
    }

    #[test]
    fn series_stream_recommendation_matches_release_family_and_preserves_exact_choice() {
        let release = |owner: &str, id: &str, title: &str| {
            let mut candidate = row(owner, id);
            candidate.choice_id = Some(id.into());
            candidate.source = StreamSource::Url(format!("https://example.invalid/{id}"));
            candidate.card.title = title.into();
            candidate
        };
        let previous = release("addon", "old", "Show.S01E01.1080p.WEB-DL.H264-GROUP");
        let preference = StreamPreference::from_row(&previous);
        let mut candidates = vec![
            release("addon", "other", "Show.S01E02.720p.HDTV.H265-OTHER"),
            release("addon", "family", "Show.S01E02.720p.WEB-DL.H264-GROUP"),
            release("addon", "closest", "Show.S01E02.1080p.WEB-DL.H264-GROUP"),
        ];
        assert_eq!(
            recommended_stream(&candidates, Some("old"), &preference),
            Some(2)
        );
        assert_eq!(
            recommended_stream(&candidates, Some("other"), &preference),
            None,
            "an exact episode selection always takes precedence"
        );
        candidates[2].owner_url = "unrelated-addon".into();
        assert_eq!(recommended_stream(&candidates, None, &preference), Some(1));
        assert_eq!(
            recommended_stream(&candidates[..1], None, &preference),
            None
        );
        let technical = release("addon", "technical", "Show.S01E02.1080p.WEB-DL.H264");
        assert_eq!(
            recommended_stream(&[technical], None, &preference),
            Some(0),
            "a missing group can still match three technical facts"
        );
        let numbered = release("addon", "anime", "Show - 08");
        let numbered_preference = StreamPreference::from_row(&numbered);
        assert_eq!(
            recommended_stream(&[numbered], None, &numbered_preference),
            None,
            "an episode number cannot masquerade as a release group"
        );
        let webdl = release("addon", "plain", "Show WEB-DL");
        let webdl_preference = StreamPreference::from_row(&webdl);
        assert_eq!(
            recommended_stream(&[webdl], None, &webdl_preference),
            None,
            "a source suffix alone cannot masquerade as a release group"
        );
        let mut binge = release("addon", "binge", "Episode title");
        binge.card.binge_group = Some("family-hash".into());
        let binge_preference = StreamPreference::from_row(&binge);
        assert_eq!(
            recommended_stream(&[binge], None, &binge_preference),
            Some(0)
        );
    }

    #[test]
    fn full_stream_snapshots_replace_one_owner_and_keep_ranked_insertions() {
        let mut other = row("other-owner", "other");
        other.addon = "Same label".into();
        let owned = |id: &str| {
            let mut row = row("owner", id);
            row.addon = "Same label".into();
            row
        };
        let mut rows = vec![other, owned("a")];
        replace_stream_snapshot(
            &mut rows,
            "owner",
            vec![owned("new"), owned("b"), owned("a")],
        );
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["other", "new", "b", "a"]
        );
        replace_stream_snapshot(&mut rows, "owner", vec![owned("a"), owned("b"), owned("b")]);
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["other", "a", "b"]
        );
        replace_stream_snapshot(&mut rows, "owner", vec![]);
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["other"]
        );
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

    #[test]
    fn last_stream_promotes_one_match_after_downloads_without_reordering_the_rest() {
        let mut download = row("", "download:one");
        download.download = Some(DownloadJob::default());
        let mut chosen = row("B", "b2");
        chosen.choice_id = Some("choice".into());
        let duplicate = chosen.clone();
        let mut rows = vec![download, row("A", "a1"), row("B", "b1"), chosen, duplicate];
        promote_last_stream(&mut rows, Some("choice"));
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["download:one", "b2", "a1", "b1", "b2"]
        );
        promote_last_stream(&mut rows, Some("missing"));
        assert_eq!(rows[0].id, "download:one");
        assert_eq!(rows[1].id, "b2");
    }
}

#[cfg(test)]
mod episode_artwork_tests {
    use super::*;
    #[test]
    fn episode_artwork_survives_native_only_refresh_without_moving_to_other_ids() {
        let cached = vec![Video {
            id: "anikoto:ep:part2-one".into(),
            season: Some(2),
            episode: Some(14),
            thumbnail: Some("https://images.example/14.jpg".into()),
            ..Default::default()
        }];
        let mut fresh = vec![
            Video {
                id: cached[0].id.clone(),
                season: Some(1),
                episode: Some(1),
                ..Default::default()
            },
            Video {
                id: "anikoto:ep:different-part-one".into(),
                season: Some(1),
                episode: Some(1),
                ..Default::default()
            },
        ];
        retain_episode_artwork(&mut fresh, &cached);
        assert_eq!(fresh[0].thumbnail, cached[0].thumbnail);
        assert_eq!(fresh[0].season, Some(1));
        assert_eq!(fresh[0].episode, Some(1));
        assert!(fresh[0].extra.is_empty()); // no stale stream aliases restored
        assert!(fresh[1].thumbnail.is_none());
        fresh[0].thumbnail = Some("https://images.example/new.jpg".into());
        retain_episode_artwork(&mut fresh, &cached);
        assert_eq!(
            fresh[0].thumbnail.as_deref(),
            Some("https://images.example/new.jpg")
        );
    }
}

#[cfg(all(test, feature = "desktop"))]
mod series_open_tests {
    use super::*;

    #[test]
    fn coverage_cache_preserves_progress_streams_and_home_counts_across_restart() {
        const ROOT: &str = "NOVA_COVERAGE_CACHE_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-coverage-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            for phase in ["seed", "refresh", "restore"] {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .env(ROOT, &root).env("NOVA_COVERAGE_PHASE", phase)
                    .args(["--exact", "app::detail::series_open_tests::coverage_cache_preserves_progress_streams_and_home_counts_across_restart", "--nocapture"])
                    .output().unwrap();
                assert!(
                    output.status.success(),
                    "{phase}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            fs::remove_dir_all(root).unwrap();
            return;
        };
        storage::init_at(&PathBuf::from(root));
        let phase = std::env::var("NOVA_COVERAGE_PHASE").unwrap();
        if phase == "seed" {
            let mut episodes = vec![Video {
                id: "tt100:1:1".into(),
                season: Some(1),
                episode: Some(1),
                name: "Opening".into(),
                ..Default::default()
            }];
            episodes.extend((1..=13).map(|n| Video {
                id: format!("kitsu:20:{n}"),
                season: Some(2),
                episode: Some(n),
                name: format!("Story {n}"),
                released: Some(format!("2020-10-{n:02}")),
                extra: std::collections::HashMap::from([(
                    "novaSourceUrl".into(),
                    serde_json::json!("https://donor.example"),
                )]),
                ..Default::default()
            }));
            write_episodes_cache_for("series", "tt100", &episodes);
            let mut progress = HashMap::new();
            Bridge::set_episode_watched_locked(&mut progress, "tt100", "tt100:1:1", true);
            for n in 1..=12 {
                Bridge::set_episode_watched_locked(
                    &mut progress,
                    "tt100",
                    &format!("kitsu:20:{n}"),
                    true,
                );
            }
            progress.insert(
                progress_map_key("tt100", "kitsu:20:13"),
                EpisodeProgress {
                    series_id: "tt100".into(),
                    episode_id: "kitsu:20:13".into(),
                    position_secs: 300.0,
                    duration_secs: 1400.0,
                    ..Default::default()
                },
            );
            write_json("test:coverage:progress", &progress);
            return;
        }
        let progress: HashMap<String, EpisodeProgress> =
            read_json("test:coverage:progress").unwrap();
        if phase == "refresh" {
            let item: MetaItem = serde_json::from_value(serde_json::json!({"id":"tt100", "type":"series", "name":"Example", "year":"2020",
                "novaMetadataRevision": nova_providers::metadata_revision(), "novaSourceUrl":"https://primary.example", "videos":[
                    {"id":"tt100:1:1", "season":1, "episode":1, "name":"Opening"},
                    {"id":"tt100:2:1", "season":2, "episode":1, "name":"Story 1", "released":"2020-10-01"}
                ]})).unwrap();
            let videos = write_episode_meta_cache_for("series", "tt100", &item);
            assert_eq!(videos.len(), 14);
            assert_eq!(videos[1].id, "kitsu:20:1");
            let aliases = confirmed_episode_stream_ids(&videos[1]);
            assert!(aliases.contains(&"tt100:2:1".into()));
            let addon = Installed { url: "https://streams.example".into(), label: "Streams".into(), enabled: true, available: true,
                configure_ok: None, generation: 0, manifest: serde_json::from_value(serde_json::json!({"id":"streams", "name":"Streams", "version":"1", "types":["series"], "resources":["stream"], "idPrefixes":["tt"]})).unwrap() };
            let url = stream_endpoint_owned(
                &addon,
                "series",
                &videos[1].id,
                None,
                &aliases,
                Some("https://donor.example"),
            )
            .unwrap();
            assert!(url.ends_with("/stream/series/tt100:2:1.json"));
        }
        let cached = read_episodes_cache_for("series", "tt100").unwrap();
        assert_eq!(cached.len(), 14);
        assert_eq!(
            library_episode_counts("tt100", &cached, &progress),
            (13, 14)
        );
        let next = next_episode_to_watch("tt100", &cached, &progress).unwrap();
        assert_eq!(next.id, "kitsu:20:13");
        assert_eq!(
            progress[&progress_map_key("tt100", &next.id)].position_secs,
            300.0
        );
        assert_eq!(
            read_json::<HashMap<String, EpisodeProgress>>("test:coverage:progress")
                .unwrap()
                .len(),
            progress.len()
        );
    }

    #[test]
    fn aliased_series_opens_episodes_and_fetches_streams_only_after_selection() {
        const ROOT: &str = "NOVA_SERIES_OPEN_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-series-open-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            for mode in ["missing", "empty", "populated"] {
                for origin in ["discover", "library"] {
                    let child_root = root.join(mode).join(origin);
                    let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .env(ROOT, &child_root)
                    .env("XDG_CACHE_HOME", child_root.join("cache"))
                    .env("NOVA_SERIES_OPEN_CACHE", mode)
                    .env("NOVA_SERIES_OPEN_ORIGIN", origin)
                    .args(["--exact", "app::detail::series_open_tests::aliased_series_opens_episodes_and_fetches_streams_only_after_selection", "--nocapture"])
                    .output().unwrap();
                    assert!(
                        output.status.success(),
                        "mode={mode} origin={origin} {}\n{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
            fs::remove_dir_all(root).unwrap();
            return;
        };
        use std::io::{Read, Write};
        let root = PathBuf::from(root);
        let mode = std::env::var("NOVA_SERIES_OPEN_CACHE").unwrap();
        let library = std::env::var("NOVA_SERIES_OPEN_ORIGIN").unwrap() == "library";
        storage::init_at(&root);
        i_slint_backend_testing::init_integration_test_with_system_time();
        let videos = vec![
            Video {
                id: "tt0994314:1:1".into(),
                season: Some(1),
                episode: Some(1),
                name: "Opening".into(),
                ..Default::default()
            },
            Video {
                id: "tt0994314:1:2".into(),
                season: Some(1),
                episode: Some(2),
                name: "Arrival".into(),
                ..Default::default()
            },
        ];
        if mode != "missing" {
            write_episodes_cache_for(
                "series",
                "kitsu:1415",
                if mode == "empty" { &[] } else { &videos },
            );
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let meta_body = serde_json::to_vec(&serde_json::json!({"meta":{
            "id":"tt0994314", "type":"series", "name":"Saved series", "_kitsuId":"1415", "imdb_id":"tt0994314", "status":"Ended", "videos": videos
        }})).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(vec![]));
        let seen = requests.clone();
        let done = stop.clone();
        let server = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
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
                    .to_owned();
                seen.lock().unwrap().push(path.clone());
                let body = if path.contains("/meta/") {
                    meta_body.as_slice()
                } else {
                    br#"{"streams":[]}"#.as_slice()
                };
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                socket.write_all(body).unwrap();
            }
        });
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let (hi, _) = mpsc::channel();
        let (lo, _) = mpsc::channel();
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        let manifest: Manifest = serde_json::from_value(serde_json::json!({"id":"metadata", "name":"Metadata", "version":"1", "types":["series"], "resources":["meta", "stream"], "idPrefixes":["kitsu:", "tt"]})).unwrap();
        bridge.shared.lock().unwrap().installed = vec![Installed {
            url: base.clone(),
            label: "Metadata".into(),
            enabled: true,
            available: true,
            configure_ok: None,
            generation: 0,
            manifest: manifest.clone(),
        }];
        net::init_metadata_transport();
        nova_providers::configure_metadata_addons(vec![nova_providers::MetadataAddon {
            url: base,
            manifest,
        }]);
        let progress_key = progress_map_key("kitsu:1415", "tt0994314:1:1");
        bridge.shared.lock().unwrap().progress.insert(
            progress_key.clone(),
            EpisodeProgress {
                series_id: "kitsu:1415".into(),
                episode_id: "tt0994314:1:1".into(),
                watched: true,
                ..Default::default()
            },
        );
        if library {
            bridge.shared.lock().unwrap().entries.push(LibraryEntry {
                id: "kitsu:1415".into(),
                type_: "series".into(),
                name: "Saved series".into(),
                year: String::new(),
                poster_url: String::new(),
                background_url: String::new(),
                genres: vec![],
                description: String::new(),
                categories: vec![],
                watch_status: WatchStatus::Auto,
                imported_tracking_status: None,
                added_at_secs: 0,
            });
            // Row actions validate against the painted Library snapshot so
            // metadata-driven collection changes cannot redirect a stale index.
            bridge.apply_library_to_ui();
            bridge.open_library_item(0);
        } else {
            bridge.open_preview(
                MetaPreview {
                    id: "kitsu:1415".into(),
                    type_: "series".into(),
                    name: "Saved series".into(),
                    ..Default::default()
                },
                0,
                false,
                None,
                false,
            );
        }
        assert!(
            bridge.shared.lock().unwrap().modal_item.is_some(),
            "the selected series must open before its metadata can load"
        );
        let started = std::time::Instant::now();
        let picked = Rc::new(std::cell::Cell::new(false));
        let selected = picked.clone();
        let timer_bridge = bridge.clone();
        let seen = requests.clone();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(20),
            move || {
                assert!(
                    started.elapsed() < Duration::from_secs(10),
                    "series never returned to its episode list"
                );
                let app = timer_bridge.app().unwrap();
                let ready = timer_bridge
                    .shared
                    .lock()
                    .unwrap()
                    .modal_item
                    .as_ref()
                    .is_some_and(|m| !m.episodes_loading);
                if !selected.get() && ready {
                    assert!(
                        app.get_modal_episodes(),
                        "a normal series open must show episodes"
                    );
                    assert_eq!(app.get_episode_rows().row_count(), 2);
                    assert!(
                        seen.lock()
                            .unwrap()
                            .iter()
                            .all(|path: &String| !path.contains("/stream/")),
                        "metadata refresh must not fetch whole-series streams"
                    );
                    selected.set(true);
                    timer_bridge.episode_picked(0);
                } else if selected.get() && !app.get_streams_searching() {
                    slint::quit_event_loop().unwrap();
                }
            },
        );
        app.run().unwrap();
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        assert!(picked.get());
        assert!(bridge.shared.lock().unwrap().progress[&progress_key].watched);
        assert_eq!(
            bridge
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .unwrap()
                .id,
            "kitsu:1415"
        );
        let paths = requests.lock().unwrap();
        assert_eq!(
            paths
                .iter()
                .filter(|p| p.contains("/stream/"))
                .collect::<Vec<_>>(),
            vec![&"/stream/series/tt0994314:1:1.json".to_string()]
        );
        assert_eq!(
            read_episodes_cache_for("series", "kitsu:1415").unwrap()[0].id,
            "tt0994314:1:1"
        );
    }
}

#[cfg(test)]
mod player_art_resolution_tests {
    use super::*;

    /// Isolate the exact CPU resample formerly done by episode selection.
    /// Explicitly include Original: it is the default and only clones pixels,
    /// so moving a resize to a worker cannot improve that case.
    #[test]
    #[ignore = "informational cached artwork resize timings; no CI frame threshold"]
    fn measure_cached_player_preview_resize_cost() {
        for (name, width, height, maximum) in [
            ("default-thumbnail-original", 360, 202, None),
            ("default-poster-original", 480, 720, None),
            ("thumbnail-cap-240", 640, 360, Some(240)),
            ("large-thumbnail-cap-720", 1920, 1080, Some(720)),
            ("large-poster-cap-720", 1440, 2160, Some(720)),
        ] {
            let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
            for (index, pixel) in pixels.make_mut_slice().iter_mut().enumerate() {
                *pixel = Rgba8Pixel::new((index % 251) as u8, (index % 239) as u8, 80, 255);
            }
            let settings = CacheSettings {
                player_episode_backdrop_max_side: maximum,
                ..CacheSettings::default()
            };
            // Discard one warm-up so allocator/code page initialization does
            // not dominate the reported median of the actual resample.
            std::hint::black_box(player_episode_preview(&pixels, &settings));
            let mut timings = Vec::new();
            for _ in 0..9 {
                let started = std::time::Instant::now();
                let preview = std::hint::black_box(player_episode_preview(
                    std::hint::black_box(&pixels),
                    std::hint::black_box(&settings),
                ));
                timings.push(started.elapsed().as_nanos());
                assert!(
                    preview.width().max(preview.height()) <= maximum.unwrap_or(width.max(height))
                );
            }
            timings.sort_unstable();
            println!(
                "PLAYER_PREVIEW_RESIZE case={name} source={width}x{height} maximum={maximum:?} median_ns={} min_ns={} max_ns={} debug_assertions={}",
                timings[timings.len() / 2],
                timings[0],
                timings[timings.len() - 1],
                cfg!(debug_assertions),
            );
        }
    }

    #[test]
    fn player_preview_obeys_all_presets_and_original_without_upscaling() {
        let source = SharedPixelBuffer::<Rgba8Pixel>::new(640, 360);
        for maximum in nova_config::IMAGE_RESOLUTION_PRESETS
            .iter()
            .copied()
            .map(Some)
            .chain([None])
        {
            let settings = CacheSettings {
                player_episode_backdrop_max_side: maximum,
                ..CacheSettings::default()
            };
            let preview = player_episode_preview(&source, &settings);
            let longest = maximum.unwrap_or(640).min(640);
            assert_eq!(preview.width(), longest);
            assert!(
                (preview.height() as f64 / preview.width() as f64 - 360.0 / 640.0).abs() < 0.005
            );
            assert_eq!(
                preview.as_bytes().len(),
                preview.width() as usize * preview.height() as usize * 4
            );
        }
    }
}

// Include the opening token: reopening the same ID must not accept a delayed
// reply from the previous modal, even when its artwork URL is unchanged.
pub(super) fn detail_artwork_identity(
    modal: &ModalItem,
    kind: &str,
    id: &str,
    url: Option<&str>,
) -> String {
    let Some(url) = url.filter(|url| !url.trim().is_empty()) else {
        return String::new();
    };
    serde_json::to_string(&(
        Arc::as_ptr(&modal.open_token) as usize,
        &modal.type_,
        &modal.id,
        kind,
        id,
        image_cache_key(url, ImageUse::EpisodeThumbnail),
    ))
    .expect("artwork identity")
}

fn retain_episode_row_artwork(row: &mut EpisodeRow, old: &EpisodeRow) {
    if !row.has_thumb
        && !row.artwork_identity.is_empty()
        && row.id == old.id
        && row.artwork_identity == old.displayed_artwork_identity
        && old.has_thumb
    {
        row.thumb = old.thumb.clone();
        row.has_thumb = true;
        row.displayed_artwork_identity = old.displayed_artwork_identity.clone();
    }
}
fn retain_season_artwork(row: &mut SeasonCard, old: &SeasonCard) {
    if !row.has_thumb
        && !row.artwork_identity.is_empty()
        && row.id == old.id
        && row.artwork_identity == old.displayed_artwork_identity
        && old.has_thumb
    {
        row.thumb = old.thumb.clone();
        row.has_thumb = true;
        row.displayed_artwork_identity = old.displayed_artwork_identity.clone();
    }
}

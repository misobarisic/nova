//! Poster / backdrop / episode-thumbnail image pipeline.
use super::*;

const ARTWORK_RECOVERY_COOLDOWN: Duration = Duration::from_secs(60);

#[cfg(test)]
thread_local! {
    static SEARCH_RECOVERY_COUNTS: std::cell::Cell<[u64; 3]> = const { std::cell::Cell::new([0; 3]) };
}
#[cfg(test)]
pub(super) fn search_recovery_counts() -> [u64; 3] {
    SEARCH_RECOVERY_COUNTS.with(std::cell::Cell::get)
}
#[cfg(test)]
fn search_recovery_count(index: usize) {
    SEARCH_RECOVERY_COUNTS.with(|counts| {
        let mut value = counts.get();
        value[index] += 1;
        counts.set(value);
    });
}

fn search_poster_slot_current(
    state: &Shared,
    generation: u64,
    index: usize,
    key: &(String, String, String),
    source: &str,
) -> bool {
    #[cfg(test)]
    search_recovery_count(0);
    !key.0.is_empty()
        && !key.1.is_empty()
        && state.search_generation == generation
        && state.search_previews.get(index).is_some_and(|meta| {
            meta.type_ == key.0
                && meta.id == key.1
                && meta
                    .extra
                    .get("novaSourceUrl")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    == source
        })
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
#[derive(Clone)]
pub(super) struct CatalogPosterRepair {
    source: nova_providers::catalog_grouping::SourceIdentity,
    generation: u64,
    original_url: String,
}

type SearchPosterSlotReceipt = (u64, usize, (String, String, String), String);

// Keep the ordinary Detail namespace rules: exact global IDs are portable
// across accepting metadata providers, while opaque IDs require their owner.
fn global_artwork_identity(id: &str) -> bool {
    let kitsu = id.strip_prefix("kitsu:").is_some_and(|id| {
        !id.is_empty()
            && id.bytes().all(|byte| byte.is_ascii_digit())
            && id.parse::<u64>().is_ok_and(|id| id > 0)
    });
    kitsu
        || nova_providers::ExternalId::parse(id).is_some()
        || nova_providers::catalog_grouping::ParentIdentity::parse(id).is_some()
}

fn artwork_metadata_sources(
    installed: &[Installed],
    type_: &str,
    id: &str,
    receipts: &[CatalogPosterRepair],
) -> VecDeque<(String, String)> {
    #[cfg(test)]
    search_recovery_count(2);
    let owner_bound = !receipts.is_empty() && !global_artwork_identity(id);
    installed
        .iter()
        .filter(|addon| {
            addon.enabled
                && addon.available
                && addon.manifest.accepts("meta", type_, id)
                && (!owner_bound
                    || receipts
                        .iter()
                        .any(|receipt| receipt.source.addon_url == addon.url))
        })
        .filter_map(|addon| Addon::new(&addon.url).ok())
        .map(|addon| (addon.meta_url(type_, id), id.into()))
        .collect()
}

fn catalog_poster_repair_receipts(
    state: &Shared,
    preview: &MetaPreview,
    generation: u64,
    original_url: &str,
) -> Vec<CatalogPosterRepair> {
    let Some(owner) = preview
        .extra
        .get("novaSourceUrl")
        .and_then(serde_json::Value::as_str)
        .filter(|owner| !owner.is_empty())
    else {
        return vec![];
    };
    state
        .catalog_raw
        .iter()
        .filter(|raw| {
            raw.id == preview.id
                && raw.type_ == preview.type_
                && raw
                    .extra
                    .get("novaSourceUrl")
                    .and_then(serde_json::Value::as_str)
                    == Some(owner)
                && raw.poster.as_deref().unwrap_or_default() == original_url
        })
        .map(|raw| CatalogPosterRepair {
            source: nova_providers::catalog_grouping::SourceIdentity {
                addon_url: owner.into(),
                media_type: raw.type_.clone(),
                id: raw.id.clone(),
            },
            generation,
            original_url: original_url.into(),
        })
        .collect()
}

fn apply_catalog_poster_repair(
    raw: &mut [MetaPreview],
    generation: u64,
    receipts: &[CatalogPosterRepair],
    url: &str,
) -> bool {
    let mut accepted = false;
    for preview in raw {
        if receipts.iter().any(|receipt| {
            receipt.generation == generation
                && receipt.source.id == preview.id
                && receipt.source.media_type == preview.type_
                && preview
                    .extra
                    .get("novaSourceUrl")
                    .and_then(serde_json::Value::as_str)
                    == Some(receipt.source.addon_url.as_str())
                && (preview.poster.as_deref().unwrap_or_default() == receipt.original_url
                    || preview.poster.as_deref() == Some(url))
        }) {
            preview.poster = Some(url.into());
            accepted = true;
        }
    }
    accepted
}

fn catalog_repaired_poster_indices(
    previews: &mut [MetaPreview],
    receipts: &[CatalogPosterRepair],
    url: &str,
) -> Vec<usize> {
    let Some(receipt) = receipts.first() else {
        return vec![];
    };
    previews
        .iter_mut()
        .enumerate()
        .filter_map(|(index, preview)| {
            apply_catalog_poster_repair(
                std::slice::from_mut(preview),
                receipt.generation,
                receipts,
                url,
            )
            .then_some(index)
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
    #[cfg(test)]
    search_recovery_count(1);
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
        || state.home_history_list.iter().any(|entry| {
            entry.type_ == type_ && entry.series_id == id && entry.poster_url == failed_url
        })
        || state.modal_item.as_ref().is_some_and(|item| {
            item.type_ == type_ && item.id == id && item.poster_url == failed_url
        });
    current && !type_.is_empty() && !id.is_empty()
}

#[cfg(test)]
fn grid_artwork_recovery_target(
    state: &mut Shared,
    type_: &str,
    id: &str,
    failed_url: &str,
) -> Option<(String, String)> {
    grid_artwork_recovery_target_for_owner(state, type_, id, failed_url, "")
}

#[cfg(test)]
fn grid_artwork_recovery_target_for_owner(
    state: &mut Shared,
    type_: &str,
    id: &str,
    failed_url: &str,
    owner: &str,
) -> Option<(String, String)> {
    if !artwork_request_current(state, type_, id, failed_url) {
        return None;
    }
    validated_artwork_recovery_target(state, type_, id, failed_url, owner)
}

// The caller holds the same Shared guard used to validate the current row.
// Rechecking every other surface here made a batch of n search rows quadratic.
fn validated_artwork_recovery_target(
    state: &mut Shared,
    type_: &str,
    id: &str,
    failed_url: &str,
    owner: &str,
) -> Option<(String, String)> {
    let target = (type_.to_owned(), id.to_owned());
    let key = (
        target.0.clone(),
        target.1.clone(),
        failed_url.into(),
        owner.into(),
    );
    let now = std::time::Instant::now();
    if state
        .artwork_recovery
        .get(&key)
        .is_some_and(|last| now.duration_since(*last) < ARTWORK_RECOVERY_COOLDOWN)
    {
        return None;
    }
    state.artwork_recovery.insert(key, now);
    Some(target)
}

// A metadata repair can publish progress before its replacement image arrives.
// Keep that unloaded row from refetching the URL whose failure started repair.
fn library_poster_request_ready(state: &Shared, entry: &LibraryPosterRequest) -> bool {
    if !state.entries.iter().any(|saved| {
        saved.type_ == entry.type_ && saved.id == entry.id && saved.poster_url == entry.poster_url
    }) {
        return false;
    }
    let key = (
        entry.type_.clone(),
        entry.id.clone(),
        entry.poster_url.clone(),
        String::new(),
    );
    !state.pending_artwork_recovery.contains_key(&key)
        && !state
            .artwork_recovery
            .get(&key)
            .is_some_and(|last| last.elapsed() < ARTWORK_RECOVERY_COOLDOWN)
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
    pub(super) fn fetch_prepared_search_poster(
        &self,
        generation: u64,
        index: usize,
        key: (String, String, String),
        source: String,
        url: String,
    ) {
        let current = {
            let state = self.shared.lock().unwrap();
            search_poster_slot_current(&state, generation, index, &key, &source)
        };
        if !current {
            return;
        }
        if url.is_empty() {
            if !key.1.starts_with("local:") {
                self.recover_missing_artwork_at_search_slot(
                    &key.0,
                    &key.1,
                    &url,
                    vec![],
                    Some((generation, index, key.clone(), source)),
                );
            }
        } else {
            self.fetch_search_card_poster(key.0, key.1, url, generation);
        }
    }
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
            .chain(app.get_home_history_archive_cards().iter())
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
        let image_revision = image_settings_revision();
        net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
            if let Some(pixels) = pixels {
                let _ = crate::app::profiles::invoke_from_event_loop(move || {
                    if image_settings_revision() != image_revision {
                        bridge.load_discover_poster(type_, id, poster_url);
                        return;
                    }
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
        self.publish_discover_poster_with_receipts(type_, id, url, pixels, &[]);
    }

    fn publish_discover_poster_with_receipts(
        &self,
        type_: &str,
        id: &str,
        url: &str,
        pixels: &SharedPixelBuffer<Rgba8Pixel>,
        receipts: &[CatalogPosterRepair],
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
            let browse = if receipts.is_empty() {
                poster_indices(&mut state.previews, type_, id, url)
            } else {
                catalog_repaired_poster_indices(&mut state.previews, receipts, url)
            };
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
                if item.id != id || item.type_ != type_ || item.poster_url == url {
                    return false;
                }
                item.poster_url = url.into();
                true
            })
        };
        if update_detail && app.get_modal_visible() {
            // Grid publication carries a grid recipe. Detail reacquires its own.
            self.load_detail_poster(url.into(), id.into());
        }
        paint_poster_rows(&app.get_catalog(), &browse, id, url, &image);
        paint_poster_rows(&app.get_search_results(), &search, id, url, &image);
        for home_cards in [
            app.get_home_catalog_cards(),
            app.get_home_history_archive_cards(),
        ] {
            for (index, mut card) in home_cards.iter().enumerate() {
                if card.id == id && card.media_type == type_ {
                    card.poster_url = url.into();
                    card.poster = image.clone();
                    card.is_loaded = true;
                    home_cards.set_row_data(index, card);
                }
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
            self.persist_poster_for(type_, id, url);
            let model = app.get_library();
            let indices = self.library_poster_indices(type_, id);
            paint_poster_rows(&model, &indices, id, url, &image);
            if changed {
                self.apply_home_to_ui();
                self.dispatch_continue_posters();
                self.dispatch_upcoming_posters();
            }
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
                && decoded_cache_get(&image_cache_key(&desired, ImageUse::DetailPoster)).is_some()
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
            if !id.starts_with("local:") {
                self.recover_missing_artwork(&type_, &id, "");
            }
            return;
        }
        let inflight_id = format!("{id}\u{1}{url}");
        {
            let mut state = self.shared.lock().unwrap();
            if state.search_generation != generation
                || !state.search_poster_inflight.insert((
                    generation,
                    type_.clone(),
                    inflight_id.clone(),
                ))
            {
                return;
            }
        }

        let app_weak = self.app.clone();
        let bridge = self.clone();
        let requested_url = url.clone();
        let image_revision = image_settings_revision();
        net::fetch_image(url, Some(DISPLAY_POSTER_SIDE), move |pixels| {
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                let index = {
                    let mut state = bridge.shared.lock().unwrap();
                    state.search_poster_inflight.remove(&(
                        generation,
                        type_.clone(),
                        inflight_id.clone(),
                    ));
                    if state.search_generation == generation {
                        state
                            .search_previews
                            .iter()
                            .enumerate()
                            .find_map(|(index, meta)| {
                                (meta.type_ == type_
                                    && meta.id == id
                                    && app_weak.upgrade().is_some_and(|app| {
                                        app.get_search_results().row_data(index).is_some_and(
                                            |row| row.poster_path.as_str() == requested_url,
                                        )
                                    }))
                                .then_some(index)
                            })
                    } else {
                        None
                    }
                };
                let Some(index) = index else { return };
                if image_settings_revision() != image_revision {
                    if app_weak.upgrade().is_some_and(|app| {
                        app.get_search_results().row_data(index).is_some_and(|row| {
                            row.id.as_str() == id && row.poster_path.as_str() == requested_url
                        })
                    }) {
                        bridge.fetch_search_card_poster(type_, id, requested_url, generation);
                    }
                    return;
                }
                if let Some(app) = app_weak.upgrade()
                    && app.get_search_results().row_data(index).is_some_and(|row| {
                        row.id.as_str() == id && row.poster_path.as_str() == requested_url
                    })
                {
                    if let Some(pixels) = pixels {
                        let model = app.get_search_results();
                        for (row_index, row) in model.iter().enumerate() {
                            if row.id.as_str() == id && row.poster_path.as_str() == requested_url {
                                app.invoke_set_search_card_poster(
                                    row_index as i32,
                                    Image::from_rgba8(pixels.clone()),
                                );
                            }
                        }
                    } else if !id.starts_with("local:") {
                        bridge.recover_missing_artwork(&type_, &id, &requested_url);
                    }
                }
            });
        });
    }

    #[cfg(feature = "desktop")]
    pub(super) fn queue_catalog_poster(
        &self,
        generation: u64,
        index: usize,
        url: String,
        priority: poster_queue::Priority,
    ) -> poster_queue::Admission {
        use poster_queue::Admission;
        if self.poster_tx.queue.is_closed() {
            return Admission::Closed;
        }
        let context = profiles::capture();
        if !profiles::is_current(&context)
            || context
                .session
                .as_ref()
                .is_some_and(|session| session.cancelled.load(Ordering::Acquire))
        {
            return Admission::Obsolete;
        }
        let settings = if profiles::enabled() {
            let Some(owner) = context
                .session
                .as_ref()
                .and_then(|session| session.providers.get())
            else {
                return Admission::Obsolete;
            };
            owner.with(nova_providers::plugin_settings::capture_settings_capability)
        } else {
            nova_providers::plugin_settings::capture_settings_capability()
        };
        let inventory = if profiles::enabled() {
            let Some(session) = context.session.as_ref() else {
                return Admission::Obsolete;
            };
            let Ok(inventory) = session.provider_inventory() else {
                return Admission::Obsolete;
            };
            inventory
        } else {
            nova_providers::OwnedProviderSnapshot::capture()
        };
        let revision = image_settings_revision();
        let (projection, media_type, id, source) = {
            let state = self.shared.lock().unwrap();
            let Some(preview) = state
                .previews
                .get(index)
                .filter(|preview| preview.poster.as_deref() == Some(url.as_str()))
            else {
                return Admission::Obsolete;
            };
            (
                catalog_grouping::display_key(preview),
                preview.type_.clone(),
                preview.id.clone(),
                preview
                    .extra
                    .get("novaSourceUrl")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            )
        };
        let authority =
            nova_media::net::ImageFetchOwner::new(settings.clone(), inventory.clone()).generation();
        let key = poster_queue::Key {
            owner: context.id.clone(),
            epoch: context.epoch,
            generation,
            revision,
            authority,
            grid: "catalog",
            media_type,
            id,
            source,
            projection,
            index,
            recipe: image_cache_key(&url, ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE))),
            url,
        };
        let job = PosterJob {
            key: key.clone(),
            context,
            settings,
            inventory,
        };
        self.poster_tx.queue.submit(key, job, priority)
    }

    /// (Re)fetch the poster for one card (scroll-back after unloading, or a
    /// not-yet-loaded row inside the ±2-row preload window). Near-viewport
    /// work goes on the high-priority channel so it jumps ahead of the
    /// background full-grid sweep.
    pub(super) fn fetch_card_poster(&self, library: bool, index: usize, url: String) {
        if library {
            self.dispatch_library_posters();
            return;
        }
        #[cfg(feature = "desktop")]
        {
            let generation = self.catalog_gen.load(Ordering::Relaxed);
            let _ =
                self.queue_catalog_poster(generation, index, url, poster_queue::Priority::Visible);
        }
        #[cfg(not(feature = "desktop"))]
        {
            let current_url = self
                .shared
                .lock()
                .unwrap()
                .previews
                .get(index)
                .is_some_and(|preview| preview.poster.as_deref() == Some(url.as_str()));
            if current_url {
                self.enqueue_poster_intake(
                    poster_intake::Surface::Catalog,
                    index,
                    true,
                    String::new(),
                );
            }
        }
    }

    pub(super) fn queue_episode_thumbnails(&self) {
        self.queue_detail_thumbnails(false, true, false);
    }

    pub(super) fn queue_episode_thumbnail_verify(&self) {
        self.queue_detail_thumbnails(true, true, false);
    }

    /// Queue just the current episode page, or the season picker. The UI can
    /// already own a successful image which never fitted the decoded cache.
    fn queue_detail_thumbnails(&self, verify: bool, episodes: bool, seasons: bool) {
        let Some(app) = self.app() else { return };
        let painted: HashSet<String> = app
            .get_episode_rows()
            .iter()
            .filter(|row| row.has_thumb)
            .map(|row| row.displayed_artwork_identity.to_string())
            .chain(
                app.get_season_cards()
                    .iter()
                    .filter(|row| row.has_thumb)
                    .map(|row| row.displayed_artwork_identity.to_string()),
            )
            .collect();
        let filter = app.get_episode_filter().to_string();
        let revision = image_settings_revision();
        let jobs = {
            let state = self.shared.lock().unwrap();
            let Some(m) = state.modal_item.as_ref() else {
                return;
            };
            let mut candidates = Vec::new();
            if episodes
                && let Some(&season) = catalog_detail::seasons(&state, m).get(m.season_index)
            {
                for v in catalog_detail::episodes(&state, m, season)
                    .into_iter()
                    .filter(|v| episode_matches_filter(v, &filter))
                    .skip(m.episode_page * EPISODE_PAGE_SIZE)
                    .take(EPISODE_PAGE_SIZE)
                {
                    let (watched, progress, _) =
                        Self::episode_watch_state(&state.progress, &m.id, &v.id);
                    if !spoiler::hidden(&state, &m.type_, &m.id, &v.id)
                        && Self::episode_thumb_visible(
                            state.cache_settings.show_unwatched_thumbs,
                            watched,
                            progress,
                        )
                        && let Some(url) =
                            v.thumbnail.as_deref().filter(|url| !url.trim().is_empty())
                    {
                        candidates.push((
                            url.to_owned(),
                            super::detail::detail_artwork_identity(m, "episode", &v.id, Some(url)),
                        ));
                    }
                }
            }
            if seasons {
                for season in catalog_detail::seasons(&state, m) {
                    if let Some(url) = catalog_detail::season_artwork(&state, m, season) {
                        let identity = super::detail::detail_artwork_identity(
                            m,
                            "season",
                            &season.to_string(),
                            Some(&url),
                        );
                        candidates.push((url, identity));
                    }
                }
            }
            let mut inflight = EPISODE_INFLIGHT.lock().unwrap();
            let mut jobs = Vec::new();
            for (url, identity) in candidates {
                let cache_key = image_cache_key(&url, ImageUse::EpisodeThumbnail);
                let loaded = painted.contains(&identity) || decoded_cache_contains(&cache_key);
                if loaded != verify {
                    continue;
                }
                let request_key =
                    format!("{:p}:{revision}:{cache_key}", Arc::as_ptr(&m.open_token));
                if inflight.contains_key(&request_key) {
                    continue;
                }
                let job_id = EPISODE_JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
                inflight.insert(request_key.clone(), job_id);
                jobs.push(EpisodeThumbJob {
                    context: profiles::capture(),
                    job_id,
                    request_key,
                    url,
                    cache_key,
                    settings_revision: revision,
                    open_token: m.open_token.clone(),
                    item_id: m.id.clone(),
                    media_type: m.type_.clone(),
                    verify,
                });
            }
            jobs
        };
        EPISODE_QUEUE.lock().unwrap().extend(jobs);
        Self::pump_episode_thumbs(self.clone());
    }

    pub(super) fn release_episode_thumb_job(job: &EpisodeThumbJob) {
        let mut inflight = EPISODE_INFLIGHT.lock().unwrap();
        if inflight.get(&job.request_key) == Some(&job.job_id) {
            inflight.remove(&job.request_key);
        }
    }

    /// Reserve a slot atomically: completions on separate transport threads
    /// may pump simultaneously, but episode and season work together stays <=4.
    pub(super) fn pump_episode_thumbs(bridge: Bridge) {
        thread_local! { static PUMPING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
        super::page_artwork::with_pump_guard(&PUMPING, || {
            loop {
                if EPISODE_ACTIVE
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |active| {
                        (active < 4).then_some(active + 1)
                    })
                    .is_err()
                {
                    break;
                }
                let Some(job) = EPISODE_QUEUE.lock().unwrap().pop_front() else {
                    EPISODE_ACTIVE.fetch_sub(1, Ordering::SeqCst);
                    break;
                };
                let current_opening = bridge
                    .shared
                    .lock()
                    .unwrap()
                    .modal_item
                    .as_ref()
                    .is_some_and(|item| {
                        Arc::ptr_eq(&item.open_token, &job.open_token)
                            && item.id == job.item_id
                            && item.type_ == job.media_type
                    });
                if !profiles::is_current(&job.context)
                    || !current_opening
                    || image_settings_revision() != job.settings_revision
                {
                    Self::release_episode_thumb_job(&job);
                    EPISODE_ACTIVE.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                let worker = bridge.clone();
                let url = job.url.clone();
                let verify = job.verify;
                let finish = move |pixels: Option<SharedPixelBuffer<Rgba8Pixel>>| {
                    // Hold the slot until publication, bounding transient completion
                    // buffers even if the UI thread is busy rebuilding metadata.
                    let fallback_job = (job.request_key.clone(), job.job_id);
                    let fallback_worker = worker.clone();
                    if profiles::invoke_account(move || {
                        if profiles::is_current(&job.context)
                            && let Some(pixels) = pixels
                        {
                            worker.publish_detail_thumbnail(&job, pixels);
                        }
                        Self::release_episode_thumb_job(&job);
                        EPISODE_ACTIVE.fetch_sub(1, Ordering::SeqCst);
                        Self::pump_episode_thumbs(worker);
                    })
                    .is_err()
                    {
                        let mut inflight = EPISODE_INFLIGHT.lock().unwrap();
                        if inflight.get(&fallback_job.0) == Some(&fallback_job.1) {
                            inflight.remove(&fallback_job.0);
                        }
                        drop(inflight);
                        EPISODE_ACTIVE.fetch_sub(1, Ordering::SeqCst);
                        Self::pump_episode_thumbs(fallback_worker);
                    }
                };
                // Copy these before moving the job into the completion callback.
                // The selected recipe is identical for the episode and season uses.
                if verify {
                    refresh_image_if_changed(url, ImageUse::EpisodeThumbnail, finish);
                } else {
                    // Cleanup owns a global slot and must run even if its profile
                    // changed. Publication above still checks the captured owner.
                    nova_media::net::fetch_image(url, ImageUse::EpisodeThumbnail, finish);
                }
            }
        });
    }

    pub(super) fn publish_detail_thumbnail(
        &self,
        job: &EpisodeThumbJob,
        pixels: SharedPixelBuffer<Rgba8Pixel>,
    ) {
        if image_settings_revision() != job.settings_revision
            || image_cache_key(&job.url, ImageUse::EpisodeThumbnail) != job.cache_key
        {
            return;
        }
        let Some(app) = self.app().filter(|app| app.get_modal_visible()) else {
            return;
        };
        let identities = {
            let state = self.shared.lock().unwrap();
            let Some(m) = state.modal_item.as_ref().filter(|m| {
                Arc::ptr_eq(&m.open_token, &job.open_token)
                    && m.id == job.item_id
                    && m.type_ == job.media_type
            }) else {
                return;
            };
            let mut identities = HashSet::new();
            for v in catalog_detail::visible_native_videos(&state, m) {
                if v.thumbnail.as_deref() != Some(&job.url) {
                    continue;
                }
                let (watched, progress, _) =
                    Self::episode_watch_state(&state.progress, &m.id, &v.id);
                if !spoiler::hidden(&state, &m.type_, &m.id, &v.id)
                    && Self::episode_thumb_visible(
                        state.cache_settings.show_unwatched_thumbs,
                        watched,
                        progress,
                    )
                {
                    identities.insert(super::detail::detail_artwork_identity(
                        m,
                        "episode",
                        &v.id,
                        Some(&job.url),
                    ));
                }
            }
            for season in catalog_detail::seasons(&state, m) {
                if catalog_detail::season_artwork(&state, m, season).as_deref() == Some(&job.url) {
                    identities.insert(super::detail::detail_artwork_identity(
                        m,
                        "season",
                        &season.to_string(),
                        Some(&job.url),
                    ));
                }
            }
            identities
        };
        let image = Image::from_rgba8(pixels);
        let episodes = app.get_episode_rows();
        for index in 0..episodes.row_count() {
            if let Some(mut row) = episodes.row_data(index)
                && identities.contains(row.artwork_identity.as_str())
            {
                row.thumb = image.clone();
                row.has_thumb = true;
                row.displayed_artwork_identity = row.artwork_identity.clone();
                episodes.set_row_data(index, row);
            }
        }
        let seasons = app.get_season_cards();
        for index in 0..seasons.row_count() {
            if let Some(mut row) = seasons.row_data(index)
                && identities.contains(row.artwork_identity.as_str())
            {
                row.thumb = image.clone();
                row.has_thumb = true;
                row.displayed_artwork_identity = row.artwork_identity.clone();
                seasons.set_row_data(index, row);
            }
        }
    }

    pub(super) fn dispatch_season_thumbs(&self) {
        self.queue_detail_thumbnails(false, false, true);
    }

    /// Disk-only work has its own two lanes. Slow HTTP requests for a previous
    /// viewport must not hold up posters already stored on this device.
    fn try_library_poster_cache(&self, entry: &LibraryPosterRequest) -> bool {
        let key = (
            entry.type_.clone(),
            entry.id.clone(),
            image_cache_key(
                &entry.poster_url,
                ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
            ),
        );
        {
            let mut state = self.shared.lock().unwrap();
            if state
                .library_poster_inflight
                .iter()
                .any(|(_, _, url)| *url == entry.poster_url)
            {
                return false;
            }
            if !state.cache_settings.cache_images
                || state.library_poster_cache_checked.contains(&key)
            {
                return false;
            }
            if state.library_poster_cache_inflight.len() >= 2
                || !state.library_poster_cache_inflight.insert(key.clone())
            {
                return true;
            }
        }
        let bridge = self.clone();
        let entry = entry.clone();
        let revision = image_settings_revision();
        thread::spawn(move || {
            let pixels = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                nova_media::cache::cached_display_pixels(
                    &entry.poster_url,
                    ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
                )
            }))
            .ok()
            .flatten();
            let _ = profiles::invoke_from_event_loop(move || {
                let mut state = bridge.shared.lock().unwrap();
                state.library_poster_cache_inflight.remove(&key);
                let current = revision == image_settings_revision()
                    && key.2
                        == image_cache_key(
                            &entry.poster_url,
                            ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
                        );
                if current {
                    state.library_poster_cache_checked.insert(key);
                }
                let window = state.library_poster_window.unwrap_or((0, 31));
                drop(state);
                if current
                    && let Some(pixels) = pixels
                    && let Some(app) = bridge.app()
                {
                    let model = app.get_library();
                    let typed_indices = bridge.library_poster_indices(&entry.type_, &entry.id);
                    let indices = requested_poster_indices(&model, &entry.id, &entry.poster_url)
                        .into_iter()
                        .filter(|index| {
                            typed_indices.contains(index)
                                && *index >= window.0
                                && *index <= window.1
                        })
                        .collect::<Vec<_>>();
                    paint_poster_rows(
                        &model,
                        &indices,
                        &entry.id,
                        &entry.poster_url,
                        &Image::from_rgba8(pixels),
                    );
                }
                bridge.dispatch_library_posters();
            });
        });
        true
    }

    /// Only the current viewport may start work. Cached pixels paint before
    /// the worker limit is considered, so returning to a row never waits for
    /// unrelated network requests that were already running during a fling.
    pub(super) fn dispatch_library_posters(&self) {
        let Some(app) = self.app() else { return };
        let model = app.get_library();
        if model.row_count() == 0 {
            return;
        }
        let (first, last, identities) = {
            let mut state = self.shared.lock().unwrap();
            state
                .library_poster_failures
                .retain(|_, failed| failed.elapsed() < Duration::from_secs(30));
            let count = model.row_count().min(state.library_displayed.len());
            if count == 0 {
                return;
            }
            let (first, last) = state.library_poster_window.unwrap_or((0, 31));
            let last = last.min(count - 1);
            let first = first.min(last);
            (first, last, state.library_displayed[first..=last].to_vec())
        };
        let center = first + (last - first) / 2;
        let mut indices = (first..=last).collect::<Vec<_>>();
        indices.sort_by_key(|index| index.abs_diff(center));
        for index in indices {
            let Some(row) = model.row_data(index).filter(|row| !row.is_loaded) else {
                continue;
            };
            let (type_, id) = &identities[index - first];
            if row.id.as_str() != id {
                continue;
            }
            let entry = LibraryPosterRequest {
                id: id.clone(),
                type_: type_.clone(),
                poster_url: row.poster_path.to_string(),
            };
            if let Some(pixels) = decoded_cache_get(&image_cache_key(
                &entry.poster_url,
                ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
            )) {
                app.invoke_set_library_poster(index as i32, Image::from_rgba8(pixels));
                continue;
            }
            if entry.poster_url.is_empty() {
                self.recover_library_artwork(&entry.type_, &entry.id, "");
                continue;
            }
            if self.try_library_poster_cache(&entry) {
                continue;
            }
            let url = entry.poster_url.clone();
            let id = entry.id.clone();
            let key = (entry.type_.clone(), id.clone(), url.clone());
            let queued = {
                let mut state = self.shared.lock().unwrap();
                state.library_poster_inflight.len() < 4
                    && !state
                        .library_poster_inflight
                        .iter()
                        .any(|(_, _, pending_url)| *pending_url == url)
                    && !state
                        .library_poster_failures
                        .get(&key)
                        .is_some_and(|failed| failed.elapsed() < Duration::from_secs(30))
                    && library_poster_request_ready(&state, &entry)
                    && state.library_poster_inflight.insert(key.clone())
            };
            if !queued {
                continue;
            }
            let bridge = self.clone();
            let image_revision = image_settings_revision();
            net::fetch_image(url.clone(), Some(DISPLAY_POSTER_SIDE), move |pixels| {
                let _ = crate::app::profiles::invoke_from_event_loop(move || {
                    let mut state = bridge.shared.lock().unwrap();
                    state.library_poster_inflight.remove(&key);
                    if pixels.is_none() {
                        state
                            .library_poster_failures
                            .insert(key.clone(), std::time::Instant::now());
                    } else {
                        state.library_poster_failures.remove(&key);
                    }
                    let current = state.entries.iter().any(|entry| {
                        entry.type_ == key.0 && entry.id == id && entry.poster_url == url
                    });
                    let window = state.library_poster_window.unwrap_or((0, 31));
                    drop(state);
                    if current
                        && image_settings_revision() == image_revision
                        && let Some(app) = bridge.app()
                    {
                        let model = app.get_library();
                        // Resolve current identity after filter changes, and
                        // never reattach textures to rows scrolled out of view.
                        let typed_indices = bridge.library_poster_indices(&key.0, &id);
                        let indices = requested_poster_indices(&model, &id, &url)
                            .into_iter()
                            .filter(|index| {
                                typed_indices.contains(index)
                                    && *index >= window.0
                                    && *index <= window.1
                            })
                            .collect::<Vec<_>>();
                        if let Some(pixels) = pixels {
                            paint_poster_rows(
                                &model,
                                &indices,
                                &id,
                                &url,
                                &Image::from_rgba8(pixels),
                            );
                        } else {
                            bridge.recover_library_artwork(&key.0, &id, &url);
                        }
                    }
                    bridge.dispatch_library_posters();
                });
            });
        }
    }

    pub(super) fn library_poster_visible_range(&self, first: usize, last: usize) {
        let Some(app) = self.app() else { return };
        let previous = {
            let mut state = self.shared.lock().unwrap();
            let previous = state.library_poster_window.replace((first, last));
            if previous != Some((first, last)) {
                state.library_poster_cache_checked.clear();
            }
            previous
        };
        if let Some((old_first, old_last)) = previous {
            let keep_first = first.saturating_sub(UNLOAD_EXTRA_CARDS);
            let keep_last = last.saturating_add(UNLOAD_EXTRA_CARDS);
            let model = app.get_library();
            for index in old_first.saturating_sub(UNLOAD_EXTRA_CARDS)
                ..=old_last
                    .saturating_add(UNLOAD_EXTRA_CARDS)
                    .min(model.row_count().saturating_sub(1))
            {
                if (index < keep_first || index > keep_last)
                    && model.row_data(index).is_some_and(|row| row.is_loaded)
                {
                    app.invoke_clear_library_poster(index as i32, Image::default());
                }
            }
        }
        self.dispatch_library_posters();
    }

    #[cfg(test)]
    pub(super) fn recover_library_artwork_for_test(&self, type_: &str, id: &str, failed_url: &str) {
        self.recover_library_artwork(type_, id, failed_url);
    }

    /// Old saved URLs can expire even when episodes/header text are cached.
    /// Keep native IDs; ask the same metadata pipeline used by Detail for art.
    fn recover_library_artwork(&self, type_: &str, id: &str, failed_url: &str) {
        let current =
            self.shared.lock().unwrap().entries.iter().any(|entry| {
                entry.type_ == type_ && entry.id == id && entry.poster_url == failed_url
            });
        if current {
            self.recover_missing_artwork(type_, id, failed_url);
        }
    }

    // Library cards carry translated type labels. The retained projection is
    // the authoritative typed identity, including after filtering/reordering.
    fn library_poster_indices(&self, type_: &str, id: &str) -> Vec<usize> {
        self.shared
            .lock()
            .unwrap()
            .library_displayed
            .iter()
            .enumerate()
            .filter_map(|(index, (row_type, row_id))| {
                (row_type == type_ && row_id == id).then_some(index)
            })
            .collect()
    }

    pub(super) fn recover_missing_artwork(&self, type_: &str, id: &str, failed_url: &str) {
        self.recover_missing_artwork_with_receipts(type_, id, failed_url, vec![]);
    }

    fn recover_missing_artwork_with_receipts(
        &self,
        type_: &str,
        id: &str,
        failed_url: &str,
        receipts: Vec<CatalogPosterRepair>,
    ) {
        self.recover_missing_artwork_at_search_slot(type_, id, failed_url, receipts, None);
    }

    fn recover_missing_artwork_at_search_slot(
        &self,
        type_: &str,
        id: &str,
        failed_url: &str,
        receipts: Vec<CatalogPosterRepair>,
        search_slot: Option<SearchPosterSlotReceipt>,
    ) {
        let target = {
            let mut state = self.shared.lock().unwrap();
            let slot_current = search_slot
                .as_ref()
                .map(|(generation, index, key, source)| {
                    search_poster_slot_current(&state, *generation, *index, key, source)
                });
            if slot_current == Some(false) {
                return;
            }
            let fast_current = search_slot.as_ref().is_some_and(|(_, index, _, _)| {
                state.search_previews[*index]
                    .poster
                    .as_deref()
                    .unwrap_or_default()
                    == failed_url
            });
            if !fast_current && !artwork_request_current(&state, type_, id, failed_url) {
                return;
            }
            let owner = receipts
                .first()
                .map_or("", |receipt| receipt.source.addon_url.as_str());
            if !receipts.is_empty()
                && !receipts.iter().any(|receipt| {
                    receipt.generation == self.catalog_gen.load(Ordering::Relaxed)
                        && state.catalog_raw.iter().any(|raw| {
                            raw.id == receipt.source.id
                                && raw.type_ == receipt.source.media_type
                                && raw
                                    .extra
                                    .get("novaSourceUrl")
                                    .and_then(serde_json::Value::as_str)
                                    == Some(receipt.source.addon_url.as_str())
                                && raw.poster.as_deref().unwrap_or_default() == receipt.original_url
                        })
                })
            {
                return;
            }
            // A pending manifest is not a failed metadata request. Don't spend
            // the cooldown before any source can service this native identity.
            if artwork_metadata_sources(&state.installed, type_, id, &receipts).is_empty() {
                if state.installed.is_empty()
                    || state
                        .installed
                        .iter()
                        .any(|addon| addon.enabled && !addon.available)
                {
                    state.pending_artwork_recovery.insert(
                        (type_.into(), id.into(), failed_url.into(), owner.into()),
                        receipts,
                    );
                }
                return;
            }
            state.pending_artwork_recovery.remove(&(
                type_.into(),
                id.into(),
                failed_url.into(),
                owner.into(),
            ));
            validated_artwork_recovery_target(&mut state, type_, id, failed_url, owner)
        };
        if let Some(target) = target {
            self.recover_artwork_target_with_header_receipt(
                target,
                failed_url,
                receipts,
                search_slot.is_some(),
            );
        }
    }

    /// Only retry observed failures/missing URLs. An unloaded row may simply
    /// be decoding or outside the viewport; neither needs metadata recovery.
    pub(super) fn retry_pending_artwork(&self) {
        let targets = std::mem::take(&mut self.shared.lock().unwrap().pending_artwork_recovery);
        for ((type_, id, url, _owner), receipts) in targets {
            self.recover_missing_artwork_with_receipts(&type_, &id, &url, receipts);
        }
    }

    /// A catalog worker's numeric slot is only valid for its original request.
    pub(super) fn recover_catalog_artwork(&self, generation: u64, index: usize, failed_url: &str) {
        if self.catalog_gen.load(Ordering::Relaxed) != generation {
            return;
        }
        let target = {
            let state = self.shared.lock().unwrap();
            state
                .previews
                .get(index)
                .filter(|preview| preview.poster.as_deref().unwrap_or_default() == failed_url)
                .map(|preview| {
                    (
                        preview.type_.clone(),
                        preview.id.clone(),
                        catalog_poster_repair_receipts(&state, preview, generation, failed_url),
                    )
                })
        };
        if let Some((type_, id, receipts)) = target
            && !receipts.is_empty()
        {
            self.recover_missing_artwork_with_receipts(&type_, &id, failed_url, receipts);
        }
    }

    fn publish_repaired_discover_poster(
        &self,
        type_: &str,
        id: &str,
        url: &str,
        pixels: &SharedPixelBuffer<Rgba8Pixel>,
        receipts: &[CatalogPosterRepair],
    ) {
        if !receipts.is_empty() {
            let mut state = self.shared.lock().unwrap();
            if !apply_catalog_poster_repair(
                &mut state.catalog_raw,
                self.catalog_gen.load(Ordering::Relaxed),
                receipts,
                url,
            ) {
                return;
            }
        }
        self.publish_discover_poster_with_receipts(type_, id, url, pixels, receipts);
    }

    fn recover_artwork_target_with_header_receipt(
        &self,
        (type_, id): (String, String),
        failed_url: &str,
        receipts: Vec<CatalogPosterRepair>,
        header_checked: bool,
    ) {
        let urls = artwork_metadata_sources(
            &self.shared.lock().unwrap().installed,
            &type_,
            &id,
            &receipts,
        );
        fn next(
            bridge: Bridge,
            type_: String,
            id: String,
            mut urls: VecDeque<(String, String)>,
            mut tried: HashSet<String>,
            receipts: Vec<CatalogPosterRepair>,
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
                        let _ = crate::app::profiles::invoke_from_event_loop(move || {
                            bridge.request_progress_projection()
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
                    next(bridge, type_, id, urls, tried, receipts);
                    return;
                };
                net::fetch_image(poster.clone(), Some(DISPLAY_POSTER_SIDE), move |pixels| {
                    if let Some(pixels) = pixels {
                        let _ = crate::app::profiles::invoke_from_event_loop(move || {
                            bridge.publish_repaired_discover_poster(
                                &type_, &id, &poster, &pixels, &receipts,
                            );
                        });
                    } else {
                        next(bridge, type_, id, urls, tried, receipts);
                    }
                });
            });
        }
        // Legacy headers are keyed by native ID, without catalog ownership.
        // An owned opaque catalog request requires its owner or a validated
        // broker connection, rather than another addon's coincidentally equal ID.
        // Prepared search rows already hydrated this URL at the guarded
        // metadata revision. An empty prepared URL proves there is no cached
        // alternative, so publication must not read the same header on the UI.
        let cached = (!header_checked && (receipts.is_empty() || global_artwork_identity(&id)))
            .then(|| read_meta_header_for(&type_, &id))
            .flatten()
            .map(|header| header.poster_url)
            .filter(|url| !url.is_empty() && url != failed_url);
        if let Some(url) = cached {
            // A previously decoded alternative can fix the card without any
            // metadata request. Never retry the URL that just failed here.
            let bridge = self.clone();
            net::fetch_image(url.clone(), Some(DISPLAY_POSTER_SIDE), move |pixels| {
                if let Some(pixels) = pixels {
                    let _ = crate::app::profiles::invoke_from_event_loop(move || {
                        bridge.publish_repaired_discover_poster(
                            &type_, &id, &url, &pixels, &receipts,
                        );
                    });
                } else {
                    next(bridge, type_, id, urls, HashSet::new(), receipts);
                }
            });
        } else {
            next(self.clone(), type_, id, urls, HashSet::new(), receipts);
        }
    }

    /// Fetch the Home card's selected thumbnail/poster off the UI thread.
    /// Guard both series and URL: the next episode can occupy the same slot.
    pub(super) fn dispatch_continue_posters(&self) {
        self.dispatch_episode_home_posters(false);
        self.dispatch_episode_home_posters(true);
    }

    fn dispatch_episode_home_posters(&self, new: bool) {
        let Some(app) = self.app() else {
            return;
        };
        let model = if new {
            app.get_home_new_episodes()
        } else {
            app.get_home_continue()
        };
        let requests = {
            let state = self.shared.lock().unwrap();
            let sources = if new {
                &state.new_episodes_list
            } else {
                &state.continue_list
            };
            model
                .iter()
                .enumerate()
                .filter(|(_, row)| {
                    !row.is_loaded
                        || decoded_cache_get(&sized_cache_key(
                            &row.art_url,
                            Some(DISPLAY_POSTER_SIDE),
                        ))
                        .is_none()
                })
                .filter_map(|(index, row)| {
                    let source = sources.iter().find(|source| {
                        source.series_id == row.id.as_str()
                            && (row.media_type.is_empty()
                                || source.type_ == row.media_type.as_str())
                            && (row.episode_id.is_empty()
                                || source.episode_id == row.episode_id.as_str())
                    })?;
                    let entry = state.entries.iter().find(|entry| {
                        entry.type_ == source.type_ && entry.id == source.series_id
                    })?;
                    Some((index, entry.poster_url.clone()))
                })
                .collect::<Vec<_>>()
        };
        for (index, fallback) in requests {
            self.enqueue_poster_intake(
                if new {
                    poster_intake::Surface::New
                } else {
                    poster_intake::Surface::Continue
                },
                index,
                true,
                fallback,
            );
        }
    }

    /// Upcoming and its selected calendar day share the same guarded image.
    pub(super) fn dispatch_upcoming_posters(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let requests = {
            let state = self.shared.lock().unwrap();
            app.get_home_upcoming()
                .iter()
                .enumerate()
                .filter(|(_, row)| {
                    !row.is_loaded
                        || decoded_cache_get(&sized_cache_key(
                            &row.art_url,
                            Some(DISPLAY_POSTER_SIDE),
                        ))
                        .is_none()
                })
                .filter_map(|(index, row)| {
                    let source = state
                        .upcoming_list
                        .get(row.index as usize)
                        .filter(|source| source.series_id == row.id.as_str())?;
                    let fallback = state
                        .entries
                        .iter()
                        .find(|entry| entry.type_ == source.type_ && entry.id == source.series_id)
                        .map(|entry| entry.poster_url.clone())
                        .unwrap_or_default();
                    Some((index, fallback))
                })
                .collect::<Vec<_>>()
        };
        for (index, fallback) in requests {
            self.enqueue_poster_intake(poster_intake::Surface::Upcoming, index, true, fallback);
        }
    }

    /// Fetch and paint the currently selected Home catalog rows. Each
    /// completion checks its current flattened index, identity and URL so a
    /// settings change or a newer catalog response cannot paint stale art.
    pub(super) fn dispatch_home_catalog_posters(&self) {
        let Some(app) = self.app() else {
            return;
        };
        for (surface, model) in [
            (
                poster_intake::Surface::HomeCatalog,
                app.get_home_catalog_cards(),
            ),
            (
                poster_intake::Surface::History,
                app.get_home_history_archive_cards(),
            ),
        ] {
            for (index, card) in model.iter().enumerate().filter(|(_, card)| !card.is_loaded) {
                if card.poster_url.is_empty() {
                    self.recover_missing_artwork(&card.media_type, &card.id, "");
                } else {
                    self.enqueue_poster_intake(surface, index, true, String::new());
                }
            }
        }
    }

    /// Record a backdrop URL on a saved library entry (no-op when unchanged
    /// or not in library). Backfills entries saved before the backdrop URL
    /// was persisted, so later reopens can start the image load without
    /// waiting for a meta fetch.
    pub(super) fn persist_backdrop_for(&self, type_: &str, id: &str, url: &str) {
        if url.is_empty() {
            return;
        }
        let Some(_reservation) = persistence::reserve(1) else {
            return;
        };
        let changed = {
            let mut state = self.shared.lock().unwrap();
            match state
                .entries
                .iter_mut()
                .find(|e| e.type_ == type_ && e.id == id)
            {
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
    pub(super) fn persist_poster_for(&self, type_: &str, id: &str, url: &str) {
        if url.is_empty() {
            return;
        }
        let Some(_reservation) = persistence::reserve(1) else {
            return;
        };
        let changed = {
            let mut state = self.shared.lock().unwrap();
            match state
                .entries
                .iter_mut()
                .find(|e| e.type_ == type_ && e.id == id)
            {
                Some(e) if e.poster_url != url => {
                    e.poster_url = url.to_string();
                    true
                }
                _ => false,
            }
        };
        if changed {
            // The successful library commit notifies retained local search;
            // queue admission alone must not publish a corpus refresh.
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
        type_: &str,
        id: &str,
        genres: &[String],
        description: &str,
        year: &str,
        overwrite: bool,
    ) {
        let Some(_reservation) = persistence::reserve(1) else {
            return;
        };
        let changed = {
            let mut state = self.shared.lock().unwrap();
            match state
                .entries
                .iter_mut()
                .find(|e| e.type_ == type_ && e.id == id)
            {
                Some(e) => merge_library_header_text(e, genres, description, year, overwrite),
                None => false,
            }
        };
        if changed {
            // The committed snapshot compares searchable fields, so a genre
            // or description-only header write does not restart local search.
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
        let cached = transparent_title_logo(decoded_cache_get(&image_cache_key(
            &url,
            ImageUse::Fixed(None),
        )));
        app.set_selected_logo(cached.map(Image::from_rgba8).unwrap_or_default());
        if url.is_empty() || app.get_selected_logo().size().width > 0 {
            return;
        }
        let bridge = self.clone();
        let revision = image_settings_revision();
        net::fetch_image(url.clone(), ImageUse::Fixed(None), move |pixels| {
            let pixels = transparent_title_logo(pixels);
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                let Some(app) = bridge.app() else { return };
                let valid = image_settings_revision() == revision
                    && app.get_modal_visible()
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

    /// The loading screen belongs to a playback request, not the current Detail
    /// opening. A late response must never decorate its replacement stream.
    pub(super) fn load_player_title_logo(
        &self,
        url: String,
        request: Option<crate::torrent::PlaybackRequest>,
    ) {
        let Some(app) = self.app() else { return };
        let cached = transparent_title_logo(decoded_cache_get(&image_cache_key(
            &url,
            ImageUse::Fixed(None),
        )));
        app.set_player_logo(cached.map(Image::from_rgba8).unwrap_or_default());
        if url.is_empty() || app.get_player_logo().size().width > 0 {
            return;
        }
        let Some(request) = request else { return };
        let bridge = self.clone();
        let revision = image_settings_revision();
        net::fetch_image(url, ImageUse::Fixed(None), move |pixels| {
            let pixels = transparent_title_logo(pixels);
            let _ = profiles::invoke_from_event_loop(move || {
                if bridge.playback_request_is_current(&request)
                    && image_settings_revision() == revision
                    && let Some(app) = bridge.app()
                {
                    app.set_player_logo(pixels.map(Image::from_rgba8).unwrap_or_default());
                }
            });
        });
    }

    fn detail_image_request(&self, id: &str) -> Option<Arc<()>> {
        self.shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .filter(|item| item.id == id)
            .map(|item| item.open_token.clone())
    }

    fn detail_image_current(
        &self,
        token: &Arc<()>,
        id: &str,
        url: &str,
        image_use: ImageUse,
        revision: u64,
    ) -> bool {
        image_settings_revision() == revision
            && self
                .shared
                .lock()
                .unwrap()
                .modal_item
                .as_ref()
                .is_some_and(|item| {
                    Arc::ptr_eq(&item.open_token, token)
                        && item.id == id
                        && match image_use {
                            ImageUse::DetailPoster => item.poster_url == url,
                            ImageUse::DetailBackdrop => item.background_url == url,
                            _ => false,
                        }
                })
    }

    /// Detail and grids load separate recipes, even when they share a source URL.
    fn load_detail_grid_poster(&self, type_: String, id: String, url: String, refresh: bool) {
        let settings = active_cache_settings();
        let grid_use = ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE));
        if let Some(pixels) = decoded_cache_get(&image_cache_key(&url, grid_use)) {
            // Refresh returns None for unchanged pixels. Rows that have not
            // painted yet still need the matching cached grid representation.
            self.publish_discover_poster(&type_, &id, &url, &pixels);
        }
        let bridge = self.clone();
        let requested_url = url.clone();
        let revision = image_settings_revision();
        let publish = move |pixels: Option<SharedPixelBuffer<Rgba8Pixel>>| {
            if let Some(pixels) = pixels {
                let _ = crate::app::profiles::invoke_from_event_loop(move || {
                    if image_settings_revision() == revision {
                        bridge.publish_discover_poster(&type_, &id, &requested_url, &pixels);
                    }
                });
            }
        };
        if refresh
            && ImageRecipe::resolve(ImageUse::DetailPoster, &settings)
                != ImageRecipe::resolve(grid_use, &settings)
        {
            refresh_image_if_changed(url, ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)), publish);
        } else {
            net::fetch_image(url, ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)), publish);
        }
    }

    pub(super) fn load_detail_backdrop(&self, url: String, item_id: String) {
        let Some(token) = self.detail_image_request(&item_id) else {
            return;
        };
        let bridge = self.clone();
        let revision = image_settings_revision();
        let requested_url = url.clone();
        net::fetch_image(url, ImageUse::DetailBackdrop, move |pixels| {
            let Some(pixels) = pixels else { return };
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                if bridge.detail_image_current(
                    &token,
                    &item_id,
                    &requested_url,
                    ImageUse::DetailBackdrop,
                    revision,
                ) && let Some(app) = bridge.app().filter(|app| app.get_modal_visible())
                {
                    app.set_selected_backdrop(Image::from_rgba8(pixels));
                }
            });
        });
    }

    pub(super) fn load_detail_poster(&self, url: String, item_id: String) {
        let Some(token) = self.detail_image_request(&item_id) else {
            return;
        };
        let media_type = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .map(|item| item.type_.clone());
        let revision = image_settings_revision();
        if let Some(pixels) = decoded_cache_get(&image_cache_key(&url, ImageUse::DetailPoster))
            && let Some(app) = self.app()
        {
            app.set_selected_poster(Image::from_rgba8(pixels));
        }
        let poster_url = url.clone();
        let bridge = self.clone();
        net::fetch_image(url, ImageUse::DetailPoster, move |pixels| {
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                if !bridge.detail_image_current(
                    &token,
                    &item_id,
                    &poster_url,
                    ImageUse::DetailPoster,
                    revision,
                ) {
                    return;
                }
                let Some(pixels) = pixels else {
                    if let Some(type_) = &media_type {
                        bridge.recover_missing_artwork(type_, &item_id, &poster_url);
                    }
                    return;
                };
                if let Some(app) = bridge.app().filter(|app| app.get_modal_visible()) {
                    app.set_selected_poster(Image::from_rgba8(pixels));
                }
                if let Some(type_) = media_type {
                    bridge.load_detail_grid_poster(type_, item_id, poster_url, false);
                }
            });
        });
    }

    pub(super) fn refresh_detail_poster(&self, url: String, item_id: String) {
        let Some(token) = self.detail_image_request(&item_id) else {
            return;
        };
        let media_type = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .map(|item| item.type_.clone());
        let revision = image_settings_revision();
        let poster_url = url.clone();
        let bridge = self.clone();
        refresh_image_if_changed(url, ImageUse::DetailPoster, move |fresh| {
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                if image_settings_revision() != revision
                    || bridge.detail_poster_stale(&item_id, &poster_url)
                {
                    return;
                }
                let Some(pixels) = fresh else {
                    if !decoded_cache_contains(&image_cache_key(
                        &poster_url,
                        ImageUse::DetailPoster,
                    )) && let Some(type_) = &media_type
                    {
                        bridge.recover_missing_artwork(type_, &item_id, &poster_url);
                    }
                    return;
                };
                if bridge.detail_image_current(
                    &token,
                    &item_id,
                    &poster_url,
                    ImageUse::DetailPoster,
                    revision,
                ) && let Some(app) = bridge.app().filter(|app| app.get_modal_visible())
                {
                    app.set_selected_poster(Image::from_rgba8(pixels));
                }
                if let Some(type_) = media_type {
                    bridge.load_detail_grid_poster(type_, item_id, poster_url, true);
                }
            });
        });
    }

    pub(super) fn refresh_detail_backdrop(&self, url: String, item_id: String) {
        let Some(token) = self.detail_image_request(&item_id) else {
            return;
        };
        let bridge = self.clone();
        let revision = image_settings_revision();
        let requested_url = url.clone();
        refresh_image_if_changed(url, ImageUse::DetailBackdrop, move |fresh| {
            let Some(pixels) = fresh else { return };
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                if bridge.detail_image_current(
                    &token,
                    &item_id,
                    &requested_url,
                    ImageUse::DetailBackdrop,
                    revision,
                ) && let Some(app) = bridge.app().filter(|app| app.get_modal_visible())
                {
                    app.set_selected_backdrop(Image::from_rgba8(pixels));
                }
            });
        });
    }
}

/// The image pipeline needs only the typed identity and the requested URL;
/// carrying a whole saved entry here would copy its unrelated metadata.
#[derive(Clone)]
pub(super) struct LibraryPosterRequest {
    pub id: String,
    pub type_: String,
    pub poster_url: String,
}

pub(crate) fn set_active_cache_settings(settings: CacheSettings) {
    // The decoded LRU budget lives in `nova-media`; the settings themselves are
    // shared through `nova-config` (read by the media/player crates).
    nova_media::cache::set_decoded_cache_budget(settings.lru_cache_mb);
    set_cache_settings(settings);
    nova_media::cache::gc::wake();
    plugin_cache::wake_gc_worker();
}

#[cfg(test)]
mod discover_artwork_tests {
    use super::*;
    #[test]
    fn prepared_search_slot_batch_keeps_recovery_guards_without_corpus_scans() {
        let addon = |enabled: bool, available: bool| Installed {
            url: "https://metadata.example".into(),
            label: "Fixture".into(),
            enabled,
            available,
            configure_ok: None,
            generation: 0,
            manifest: serde_json::from_value(serde_json::json!({
                "id":"fixture", "name":"Fixture", "version":"1", "types":["series"],
                "resources":["meta"], "catalogs":[]
            }))
            .unwrap(),
        };
        for enabled in [false, true] {
            let mut state = Shared {
                search_generation: 9,
                installed: vec![addon(enabled, true)],
                ..Default::default()
            };
            state.search_previews = (0..100)
                .map(|at| {
                    let mut meta = MetaPreview {
                        id: format!("native:{at}"),
                        type_: "series".into(),
                        ..Default::default()
                    };
                    meta.extra
                        .insert("novaSourceUrl".into(), "https://owner.example".into());
                    meta
                })
                .collect();
            let before = search_recovery_counts();
            let mut eligible = 0;
            for at in 0..100 {
                let key = (
                    "series".into(),
                    format!("native:{at}"),
                    "https://owner.example".into(),
                );
                assert!(search_poster_slot_current(
                    &state,
                    9,
                    at,
                    &key,
                    "https://owner.example"
                ));
                assert!(!search_poster_slot_current(
                    &state,
                    8,
                    at,
                    &key,
                    "https://owner.example"
                ));
                assert!(!search_poster_slot_current(
                    &state,
                    9,
                    at,
                    &key,
                    "https://other.example"
                ));
                let sources = artwork_metadata_sources(&state.installed, &key.0, &key.1, &[]);
                if !sources.is_empty() {
                    eligible += 1;
                    assert!(
                        validated_artwork_recovery_target(&mut state, &key.0, &key.1, "", "")
                            .is_some()
                    );
                    assert!(
                        validated_artwork_recovery_target(&mut state, &key.0, &key.1, "", "")
                            .is_none()
                    );
                    // A different failed URL remains independently retryable.
                    assert!(
                        validated_artwork_recovery_target(&mut state, &key.0, &key.1, "failed", "")
                            .is_some()
                    );
                }
            }
            let after = search_recovery_counts();
            assert_eq!(eligible, if enabled { 100 } else { 0 });
            assert_eq!(after[0] - before[0], 300);
            assert_eq!(after[1] - before[1], 0);
            assert_eq!(after[2] - before[2], 100);
            // Pending manifests must leave the native identity eligible later;
            // source discovery itself never consumes a request cooldown.
            let pending = vec![addon(true, false)];
            assert!(artwork_metadata_sources(&pending, "series", "new", &[]).is_empty());
            assert!(!state.artwork_recovery.keys().any(|key| key.1 == "new"));
            assert_eq!(
                artwork_metadata_sources(&[addon(true, true)], "series", "new", &[]).len(),
                1
            );
        }
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn home_paints_posters_cached_after_blank_rows_are_created_without_scrolling() {
        const ROOT: &str = "NOVA_HOME_CACHED_POSTER_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-home-posters-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .args([
                    "--exact",
                    "app::posters::discover_artwork_tests::home_paints_posters_cached_after_blank_rows_are_created_without_scrolling",
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
        set_active_cache_settings(CacheSettings {
            cache_images: false,
            ..Default::default()
        });
        i_slint_backend_testing::init_integration_test_with_system_time();
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx::closed(),
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        let rows = (0..6)
            .map(|i| ContinueRow {
                id: format!("home-{i}").into(),
                art_url: format!("https://example.invalid/home-cached-{i}.png").into(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        {
            let mut state = bridge.shared.lock().unwrap();
            state.entries = rows.iter().map(|row| serde_json::from_value::<LibraryEntry>(serde_json::json!({
                "id": row.id.to_string(), "type_": "series", "name": row.id.to_string(), "poster_url": row.art_url.to_string()
            })).unwrap()).collect();
            state.continue_list = rows[..2]
                .iter()
                .map(|row| ContinueEntry {
                    series_id: row.id.to_string(),
                    type_: "series".into(),
                    episode_id: row.id.to_string(),
                    ..Default::default()
                })
                .collect();
            state.new_episodes_list = rows[2..4]
                .iter()
                .map(|row| ContinueEntry {
                    series_id: row.id.to_string(),
                    type_: "series".into(),
                    episode_id: row.id.to_string(),
                    ..Default::default()
                })
                .collect();
            state.upcoming_list = rows[4..]
                .iter()
                .map(|row| UpcomingEntry {
                    series_id: row.id.to_string(),
                    type_: "series".into(),
                    episode_id: row.id.to_string(),
                    ..Default::default()
                })
                .collect();
        }
        app.set_home_continue(Rc::new(VecModel::from(rows[..2].to_vec())).into());
        app.set_home_new_episodes(Rc::new(VecModel::from(rows[2..4].to_vec())).into());
        let upcoming = rows[4..]
            .iter()
            .enumerate()
            .map(|(index, row)| UpcomingRow {
                index: index as i32,
                id: row.id.clone(),
                art_url: row.art_url.clone(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        app.set_home_upcoming(Rc::new(VecModel::from(upcoming.clone())).into());
        app.set_home_cal_day(Rc::new(VecModel::from(upcoming)).into());
        // A Library worker can warm shared pixels after Home's blank models
        // were constructed but before their artwork requests are dispatched.
        for row in rows {
            decoded_cache_insert(
                &sized_cache_key(&row.art_url, Some(DISPLAY_POSTER_SIDE)),
                SharedPixelBuffer::new(2, 3),
            );
        }
        bridge.dispatch_continue_posters();
        bridge.dispatch_upcoming_posters();
        let loaded = |app: &AppWindow| {
            app.get_home_continue().iter().all(|row| row.is_loaded)
                && app.get_home_new_episodes().iter().all(|row| row.is_loaded)
                && app.get_home_upcoming().iter().all(|row| row.is_loaded)
                && app.get_home_cal_day().iter().all(|row| row.is_loaded)
        };
        let started = std::time::Instant::now();
        let weak = app.as_weak();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(10),
            move || {
                if loaded(&weak.upgrade().unwrap()) || started.elapsed() > Duration::from_secs(2) {
                    slint::quit_event_loop().unwrap();
                }
            },
        );
        app.run().unwrap();
        timer.stop();
        assert!(
            loaded(&app),
            "stationary Home must paint every cached poster"
        );
        assert_eq!(
            app.get_home_continue()
                .row_data(1)
                .unwrap()
                .poster
                .size()
                .width,
            2
        );
        assert_eq!(
            app.get_home_upcoming()
                .row_data(1)
                .unwrap()
                .poster
                .size()
                .height,
            3
        );
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn library_viewport_cache_bypasses_busy_workers_and_requests_stay_bounded() {
        const ROOT: &str = "NOVA_LIBRARY_POSTER_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-library-posters-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root).env("XDG_CACHE_HOME", root.join("cache"))
                .args(["--exact", "app::posters::discover_artwork_tests::library_viewport_cache_bypasses_busy_workers_and_requests_stay_bounded", "--nocapture"])
                .output().unwrap();
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
        i_slint_backend_testing::init_integration_test_with_system_time();
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx::closed(),
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        // Keep these four local requests blocked until the assertions finish.
        // No addon or public network endpoint is involved in this fixture.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let entries = (0..120).map(|number| serde_json::from_value::<LibraryEntry>(serde_json::json!({
            "id":format!("show-{number}"), "type_":"series", "name":format!("Show {number}"), "poster_url":format!("{base}/{number}.png")
        })).unwrap()).collect::<Vec<_>>();
        app.set_library(
            Rc::new(VecModel::from(
                entries
                    .iter()
                    .map(|entry| MediaCard {
                        id: entry.id.clone().into(),
                        poster_path: entry.poster_url.clone().into(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            state.library_displayed = entries
                .iter()
                .map(|entry| (entry.type_.clone(), entry.id.clone()))
                .collect();
            state.entries = entries.clone();
            state.library_poster_window = Some((90, 99));
            state.library_poster_inflight = (0..4)
                .map(|number| ("series".into(), format!("busy-{number}"), "busy".into()))
                .collect();
            state.library_poster_cache_checked = (91..=99)
                .filter(|number| *number != 95)
                .map(|number| {
                    (
                        "series".into(),
                        format!("show-{number}"),
                        image_cache_key(
                            &entries[number].poster_url,
                            ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
                        ),
                    )
                })
                .collect();
        }
        let mut png = std::io::Cursor::new(vec![]);
        image::DynamicImage::new_rgba8(2, 3)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        assert!(
            nova_media::cache::cached_pixels_with(
                &poster_cache_dir(),
                &entries[95].poster_url,
                ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
                &nova_config::active_cache_settings(),
                || Some(png.into_inner())
            )
            .is_some()
        );
        decoded_cache_clear();
        let pixels = SharedPixelBuffer::<Rgba8Pixel>::new(2, 3);
        for number in [0, 90] {
            decoded_cache_insert(
                &image_cache_key(
                    &entries[number].poster_url,
                    ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
                ),
                pixels.clone(),
            );
        }
        bridge.dispatch_library_posters();
        assert!(
            app.get_library().row_data(90).unwrap().is_loaded,
            "cache hit must paint despite four blocked workers"
        );
        assert!(
            !app.get_library().row_data(0).unwrap().is_loaded,
            "offscreen cache hits must not allocate textures"
        );
        let started = std::time::Instant::now();
        let weak = app.as_weak();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(10),
            move || {
                if weak
                    .upgrade()
                    .unwrap()
                    .get_library()
                    .row_data(95)
                    .unwrap()
                    .is_loaded
                    || started.elapsed() > Duration::from_secs(3)
                {
                    slint::quit_event_loop().unwrap();
                }
            },
        );
        app.run().unwrap();
        timer.stop();
        assert!(
            app.get_library().row_data(95).unwrap().is_loaded,
            "disk hit must bypass the four blocked network lanes too"
        );
        bridge
            .shared
            .lock()
            .unwrap()
            .library_poster_inflight
            .clear();
        bridge.dispatch_library_posters();
        {
            let state = bridge.shared.lock().unwrap();
            assert_eq!(
                state.library_poster_inflight.len(),
                4,
                "large viewport still has a four-worker ceiling"
            );
            assert!(
                state
                    .library_poster_inflight
                    .iter()
                    .all(|(_, id, _)| (91..=99).any(|number| *id == format!("show-{number}")))
            );
        }
        bridge.library_poster_visible_range(0, 5);
        assert!(
            app.get_library().row_data(0).unwrap().is_loaded,
            "returning viewport wins over stale requests"
        );
        assert!(
            !app.get_library().row_data(90).unwrap().is_loaded,
            "old viewport releases its texture"
        );
        assert_eq!(
            bridge.shared.lock().unwrap().library_poster_inflight.len(),
            4
        );
        drop(listener);
    }
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
            for mode in ["0", "1", "2", "3", "4", "5", "6", "7"] {
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
        let detail = mode == "3" || mode == "6";
        let cached_alternative = mode == "4";
        let pending_failure = mode == "5" || mode == "7";
        let library = mode == "2" || detail || cached_alternative || mode == "5";
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
        let (png_width, png_height) = if mode == "6" { (640, 360) } else { (1, 1) };
        image::DynamicImage::new_rgba8(png_width, png_height)
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
                    } else if path.starts_with("/first/") {
                        // An explicit current broker receipt authorizes the
                        // second owner, including equal opaque native IDs.
                        // Equality of "saved" alone must never grant that.
                        meta["novaMetadataRevision"] =
                            serde_json::json!(nova_providers::metadata_revision());
                        meta["novaConnections"] = serde_json::json!([{
                            "addonId":nova_providers::addon_metadata_id(&format!("{server_base}/second")),
                            "mediaId":if mapped { "tt5095466" } else { "saved" }, "ids":[], "basis":"exact-title-year"
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
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx::closed(),
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
                imported_tracking_status: None,
                added_at_secs: 0,
            });
            state.installed = ["first", "second"].map(|path| Installed { url:format!("{base}/{path}"), label:path.into(), enabled:true,
                configure_ok:None, available:true, generation:0,
                manifest:serde_json::from_value(serde_json::json!({"id":path, "name":path, "version":"1", "types":[media_type],
                    "resources":[{"name":"meta", "types":[media_type], "idPrefixes": if mapped && path == "second" { vec!["tt"] } else { vec!["saved"] }}], "catalogs":[]})).unwrap()
            }).to_vec();
            // Discover responses now require the exact selected catalog owner.
            // Exercise the real selected-catalog path rather than publishing
            // an unowned page into an otherwise metadata-only fixture.
            let owner = &mut state.installed[0];
            let mut manifest = serde_json::to_value(&owner.manifest).unwrap();
            manifest["resources"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!("catalog"));
            manifest["catalogs"] =
                serde_json::json!([{"id":"artwork","name":"Artwork","type":media_type}]);
            owner.manifest = serde_json::from_value(manifest).unwrap();
            let type_defs = catalog::build_type_defs(&owner.manifest, &owner.label, &owner.url);
            state.type_defs = type_defs;
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
                if mode == "6" {
                    let current = active_cache_settings();
                    set_active_cache_settings(CacheSettings {
                        detail_poster_max_side: Some(240),
                        ..current
                    });
                }
                // The timer opens Detail after the async Library publication
                // has installed the row's typed action identity.
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
                // Keep the exact prepared identity/title/URL so the async
                // Library projection takes its in-place update path. Clearing
                // the image cache exposes an expired URL that must still retry.
                decoded_cache_clear();
                app.set_library(
                    Rc::new(VecModel::from(vec![MediaCard {
                        id: "saved".into(),
                        title: "Saved".into(),
                        poster_path: old.clone().into(),
                        ..Default::default()
                    }]))
                    .into(),
                );
                bridge.show_library_page();
                bridge.show_library_page();
                // Dispatch happens after the coalesced worker publishes. The
                // assertions after the event loop verify exactly one expired
                // image request and metadata recovery across both visits.
            }
        } else {
            if pending_failure {
                for addon in &mut bridge.shared.lock().unwrap().installed {
                    addon.available = false;
                }
            }
            let mut preview = preview;
            if mode == "1" {
                preview.poster = Some(format!("{base}/bad.png"));
            }
            bridge.apply_catalog(0, vec![preview], false);
            if mode == "1" {
                // Desktop poster jobs normally report this failed decode.
                // This fixture's queue is intentionally detached from workers.
                bridge.recover_catalog_artwork(0, 0, &format!("{base}/bad.png"));
            }
        }
        let weak = app.as_weak();
        let started = std::time::Instant::now();
        let expected_poster = good.clone();
        let retry_bridge = bridge.clone();
        let mut detail_opened = !detail;
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
                let app = weak.upgrade().unwrap();
                if !detail_opened {
                    let published = retry_bridge
                        .shared
                        .lock()
                        .unwrap()
                        .library_displayed
                        .first()
                        .is_some_and(|(type_, id)| type_ == media_type && id == "saved");
                    if published {
                        // A healthy movie still refreshes metadata when the
                        // user opens its published Library card. A later stale
                        // projection must not close this newly opened Detail.
                        retry_bridge.open_library_item(0);
                        assert!(
                            app.get_modal_visible(),
                            "published Library card opens Detail"
                        );
                        detail_opened = true;
                    }
                }
                // Detail and grids now publish separate image-use requests.
                // A metadata URL update alone does not finish the grid decode.
                if [
                    app.get_catalog(),
                    app.get_search_results(),
                    app.get_library(),
                ]
                .iter()
                .all(|model| {
                    model.row_data(0).is_some_and(|row| {
                        row.is_loaded && row.poster_path.as_str() == expected_poster
                    })
                }) || started.elapsed() > Duration::from_secs(10)
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
            if mode == "6" {
                let selected = app.get_selected_poster().size();
                assert_eq!((selected.width, selected.height), (240, 135));
                for model in [
                    app.get_catalog(),
                    app.get_search_results(),
                    app.get_library(),
                ] {
                    let row = model.row_data(0).unwrap();
                    let size = row.poster.size();
                    assert_eq!((size.width, size.height), (480, 270));
                }
            } else {
                assert!(app.get_selected_poster().size().width > 0);
            }
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
        if !library {
            {
                let state = bridge.shared.lock().unwrap();
                assert_eq!(state.catalog_raw[0].poster.as_deref(), Some(good.as_str()));
                assert_eq!(
                    state.catalog_raw[0].extra["novaSourceUrl"],
                    format!("{base}/first")
                );
            }
            bridge.reproject_catalog_grouping_discover();
            let row = app.get_catalog().row_data(0).unwrap();
            assert!(
                row.is_loaded,
                "reprojection retains repaired decoded artwork"
            );
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
                details: Default::default(),
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
            imported_tracking_status: None,
            added_at_secs: 0,
        });
        let old = state.entries[0].poster_url.clone();
        let mut request = LibraryPosterRequest {
            type_: "series".into(),
            id: "native:old-show".into(),
            poster_url: old.clone(),
        };
        assert!(library_poster_request_ready(&state, &request));
        let pending = (
            request.type_.clone(),
            request.id.clone(),
            old.clone(),
            String::new(),
        );
        state
            .pending_artwork_recovery
            .insert(pending.clone(), vec![]);
        assert!(!library_poster_request_ready(&state, &request));
        state.pending_artwork_recovery.remove(&pending);
        assert!(library_poster_request_ready(&state, &request));
        assert_eq!(
            grid_artwork_recovery_target(&mut state, "series", "native:old-show", &old),
            Some(("series".into(), "native:old-show".into()))
        );
        assert!(
            !library_poster_request_ready(&state, &request),
            "progress publication must wait for replacement artwork"
        );
        // Library and Home can fail together; share one metadata request.
        assert!(
            grid_artwork_recovery_target(&mut state, "series", "native:old-show", &old).is_none()
        );
        // A temporary offline failure can retry on a later page refresh.
        state.artwork_recovery.values_mut().for_each(|last| {
            *last -= Duration::from_secs(61);
        });
        assert!(library_poster_request_ready(&state, &request));
        assert!(
            grid_artwork_recovery_target(&mut state, "series", "native:old-show", &old).is_some()
        );
        state.entries[0].poster_url = "https://new.example/poster.jpg".into();
        assert!(!library_poster_request_ready(&state, &request), "stale URL");
        request.poster_url = state.entries[0].poster_url.clone();
        assert!(
            library_poster_request_ready(&state, &request),
            "new URL bypasses old repair"
        );
        request.type_ = "movie".into();
        assert!(
            !library_poster_request_ready(&state, &request),
            "type remains scoped"
        );
        assert!(
            grid_artwork_recovery_target(&mut state, "series", "native:old-show", &old).is_none()
        );
        assert!(grid_artwork_recovery_target(&mut state, "series", "removed", &old).is_none());
    }

    #[test]
    fn catalog_only_global_owner_retains_exact_id_metadata_and_header_fallback() {
        let addon = |url: &str, resources: serde_json::Value| Installed {
            url: url.into(),
            label: "same label".into(),
            enabled: true,
            available: true,
            configure_ok: None,
            generation: 0,
            manifest: serde_json::from_value(serde_json::json!({
                "id":url, "name":"Fixture", "version":"1", "types":["series"],
                "resources":resources, "catalogs":[]
            }))
            .unwrap(),
        };
        let installed = vec![
            addon("https://catalog-only", serde_json::json!(["catalog"])),
            addon("https://metadata", serde_json::json!(["meta"])),
        ];
        let mut raw = MetaPreview {
            id: "tt123".into(),
            type_: "series".into(),
            ..Default::default()
        };
        raw.extra.insert(
            "novaSourceUrl".into(),
            serde_json::json!("https://catalog-only"),
        );
        let mut state = Shared::default();
        state.catalog_raw = vec![raw.clone()];
        let receipts = catalog_poster_repair_receipts(&state, &raw, 1, "");
        let endpoints = artwork_metadata_sources(&installed, "series", "tt123", &receipts);
        assert_eq!(endpoints.len(), 1);
        assert!(
            endpoints[0]
                .0
                .starts_with("https://metadata/meta/series/tt123")
        );
        assert!(
            global_artwork_identity("tt123"),
            "global IDs may also reuse the legacy exact-ID header"
        );
        assert!(apply_catalog_poster_repair(
            &mut state.catalog_raw,
            1,
            &receipts,
            "repaired"
        ));
        assert_eq!(
            state.catalog_raw[0].extra["novaSourceUrl"],
            "https://catalog-only"
        );
        for id in [
            "tt123",
            "tmdb:tv:123",
            "tmdb:movie:123",
            "kitsu:123",
            "mal:123",
            "anilist:123",
        ] {
            assert!(global_artwork_identity(id), "{id}");
        }
        for id in ["opaque", "123", "tt", "kitsu:0", "kitsu:x"] {
            assert!(!global_artwork_identity(id), "{id}");
            let mut opaque = receipts.clone();
            opaque[0].source.id = id.into();
            assert!(artwork_metadata_sources(&installed, "series", id, &opaque).is_empty());
        }
    }

    #[test]
    fn catalog_artwork_receipts_bind_owner_native_identity_url_and_generation() {
        let preview = |owner: &str, type_: &str, id: &str, poster: &str| {
            let mut preview = MetaPreview {
                id: id.into(),
                type_: type_.into(),
                poster: Some(poster.into()),
                ..Default::default()
            };
            preview
                .extra
                .insert("novaSourceUrl".into(), serde_json::json!(owner));
            preview
        };
        let mut state = Shared::default();
        state.catalog_raw = vec![
            preview("https://first", "series", "opaque", "failed"),
            preview("https://second", "series", "opaque", "failed"),
            preview("https://first", "movie", "opaque", "failed"),
            preview("https://first", "series", "other", "failed"),
        ];
        let original = state.catalog_raw[0].clone();
        let receipts = catalog_poster_repair_receipts(&state, &original, 4, "failed");
        assert_eq!(receipts.len(), 1);
        let unchanged = serde_json::to_value(&state.catalog_raw).unwrap();
        assert!(!apply_catalog_poster_repair(
            &mut state.catalog_raw,
            5,
            &receipts,
            "good"
        ));
        assert_eq!(serde_json::to_value(&state.catalog_raw).unwrap(), unchanged);
        state.catalog_raw[0].poster = Some("newer".into());
        assert!(!apply_catalog_poster_repair(
            &mut state.catalog_raw,
            4,
            &receipts,
            "good"
        ));
        state.catalog_raw[0].poster = Some("failed".into());
        assert!(apply_catalog_poster_repair(
            &mut state.catalog_raw,
            4,
            &receipts,
            "good"
        ));
        assert_eq!(state.catalog_raw[0].poster.as_deref(), Some("good"));
        for raw in &state.catalog_raw[1..] {
            assert_eq!(raw.poster.as_deref(), Some("failed"));
        }
        assert!(
            state
                .catalog_raw
                .iter()
                .all(|raw| !raw.extra.contains_key("novaCatalogDisplayKey"))
        );
        let mut visible = vec![
            preview("https://second", "series", "opaque", "failed"),
            original.clone(),
            original,
        ];
        visible[1].extra.insert(
            "novaCatalogDisplayKey".into(),
            serde_json::json!("season-one"),
        );
        visible[2].extra.insert(
            "novaCatalogDisplayKey".into(),
            serde_json::json!("season-two"),
        );
        assert_eq!(
            catalog_repaired_poster_indices(&mut visible, &receipts, "good"),
            vec![1, 2]
        );
        assert_eq!(visible[0].poster.as_deref(), Some("failed"));
        assert_eq!(visible[1].extra["novaCatalogDisplayKey"], "season-one");
        assert_eq!(visible[2].extra["novaCatalogDisplayKey"], "season-two");
        state.previews = visible;
        assert!(
            grid_artwork_recovery_target_for_owner(
                &mut state,
                "series",
                "opaque",
                "failed",
                "https://first"
            )
            .is_some()
        );
        assert!(
            grid_artwork_recovery_target_for_owner(
                &mut state,
                "series",
                "opaque",
                "failed",
                "https://second"
            )
            .is_some()
        );
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
            && pixels
                .as_bytes()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] > 0)
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
        assert!(transparent_title_logo(Some(SharedPixelBuffer::<Rgba8Pixel>::new(2, 2))).is_none());
    }
}

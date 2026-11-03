//! Async application admission in front of MED-07's existing artwork workers.
//! These are delivery leases, not additional fetch/decode threads.
use super::*;
use nova_media::net::{ImageFetchFailure, ImageFetchOwner};
use std::time::Instant;

pub(super) const VISIBLE: usize = 128;
const BACKGROUND: usize = 256;
const DELIVERIES: usize = nova_media::net::ARTWORK_WORKERS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Surface {
    Catalog,
    Continue,
    New,
    Upcoming,
    HomeCatalog,
    History,
}
/// Top-level navigation focus, independent of transport/profile ownership.
#[derive(Clone, Copy)]
pub(super) enum Foreground {
    Home,
    Catalog,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    profile: String,
    epoch: u64,
    surface: Surface,
    generation: u64,
    index: usize,
    media_type: String,
    id: String,
    source: String,
    projection: String,
    url: String,
    fallback: String,
    recipe: String,
    authority: (u64, u64, Arc<str>),
}
/// One synchronous UI admission batch. Authority is built only after a valid
/// row is found; cloning the captured snapshot does not rebuild manifest JSON.
#[derive(Default)]
pub(super) struct Batch(std::cell::OnceCell<Option<OwnedBatch>>);

struct OwnedBatch {
    context: profiles::Context,
    owner: ImageFetchOwner,
}

#[cfg(test)]
static OWNER_CAPTURES: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct Request {
    key: Key,
    context: profiles::Context,
    owner: ImageFetchOwner,
    visible: bool,
    fallback_attempt: bool,
}
struct Active {
    ticket: u64,
    live: Arc<AtomicBool>,
    handle: Option<nova_media::net::FetchHandle>,
}
#[derive(Default)]
struct State {
    high: VecDeque<Request>,
    low: VecDeque<Request>,
    active: HashMap<Key, Active>,
    failures: VecDeque<(Key, Instant)>,
    closed: bool,
    pumping: bool,
    ticket: u64,
    refresh: bool,
    deferred: bool,
    busy_revision: Option<u64>,
    recipe_revision: Option<u64>,
}
#[derive(Default)]
pub(super) struct Intake(Mutex<State>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Admission {
    Queued,
    Promoted,
    AlreadyActive,
    DeferredCapacity,
    CoolingDown,
    Obsolete,
    Closed,
}

impl Intake {
    /// A hidden page must not fill the foreground lane ahead of the new page.
    /// Keep admitted physical work and background prefetch; returning navigation
    /// reconstructs discarded visible requests from its current unloaded rows.
    pub fn focus(&self, foreground: Foreground) {
        let removed = {
            let mut state = self.0.lock().unwrap();
            let mut removed = Vec::new();
            let high = std::mem::take(&mut state.high);
            for request in high {
                let catalog = request.key.surface == Surface::Catalog;
                if catalog == matches!(foreground, Foreground::Catalog) {
                    state.high.push_back(request);
                } else {
                    removed.push(request);
                }
            }
            state.refresh = true;
            removed
        };
        // Captured owner destructors must never run under the scheduler mutex.
        drop(removed);
    }
    fn submit(&self, request: Request) -> Admission {
        let mut state = self.0.lock().unwrap();
        if state.closed {
            return Admission::Closed;
        }
        if state.active.contains_key(&request.key) {
            return Admission::AlreadyActive;
        }
        state
            .failures
            .retain(|(_, at)| at.elapsed() < Duration::from_secs(30));
        if state.failures.iter().any(|(key, _)| key == &request.key) {
            return Admission::CoolingDown;
        }
        if state.high.iter().any(|old| old.key == request.key) {
            return Admission::Queued;
        }
        if let Some(index) = state.low.iter().position(|old| old.key == request.key) {
            if !request.visible {
                return Admission::Queued;
            }
            if state.high.len() == VISIBLE {
                state.deferred = true;
                return Admission::DeferredCapacity;
            }
            let mut old = state.low.remove(index).unwrap();
            old.visible = true;
            state.high.push_back(old);
            return Admission::Promoted;
        }
        if (request.visible && state.high.len() == VISIBLE)
            || (!request.visible && state.low.len() == BACKGROUND)
        {
            state.deferred |= request.visible;
            return Admission::DeferredCapacity;
        }
        if request.visible {
            state.high.push_back(request);
        } else {
            state.low.push_back(request);
        }
        Admission::Queued
    }
    fn needs_pump(&self, revision: impl FnOnce() -> u64) -> bool {
        let state = self.0.lock().unwrap();
        !state.closed
            && (state
                .recipe_revision
                .is_some_and(|before| before != image_settings_revision())
                || (state.active.len() < DELIVERIES
                    && (state.refresh
                        || state.deferred
                        || !state.high.is_empty()
                        || !state.low.is_empty())
                    && state
                        .busy_revision
                        .is_none_or(|before| before != revision())))
    }
    fn begin_pump(&self, revision: impl FnOnce() -> u64) -> bool {
        let mut state = self.0.lock().unwrap();
        if state.closed || state.pumping {
            return false;
        }
        if state
            .recipe_revision
            .is_some_and(|before| before != image_settings_revision())
        {
            state.busy_revision = None;
        }
        if state
            .busy_revision
            .is_some_and(|before| before == revision())
        {
            return false;
        }
        state.busy_revision = None;
        state.pumping = true;
        true
    }
    /// Called by existing catalog/profile invalidation hooks. Drop retained
    /// owners outside the mutex; running work keeps its delivery lease.
    pub fn invalidate(&self) {
        let (high, low, handles) = {
            let mut state = self.0.lock().unwrap();
            let handles = state
                .active
                .values_mut()
                .filter_map(|entry| {
                    entry.live.store(false, Ordering::Release);
                    entry.handle.take()
                })
                .collect::<Vec<_>>();
            state.refresh = true;
            state.busy_revision = None;
            (
                std::mem::take(&mut state.high),
                std::mem::take(&mut state.low),
                handles,
            )
        };
        for handle in handles {
            handle.cancel();
        }
        drop((high, low));
    }
    pub fn close(&self) {
        self.0.lock().unwrap().closed = true;
        self.invalidate();
    }
}

/// Retained through the final UI closure. A rejected dispatch, cancellation or
/// panic drops this guard too, bounding decoded UI-delivery payloads.
struct Delivery {
    intake: Arc<Intake>,
    key: Key,
    ticket: u64,
}
impl Delivery {
    fn is_live(&self) -> bool {
        let state = self.intake.0.lock().unwrap();
        !state.closed
            && state.active.get(&self.key).is_some_and(|active| {
                active.ticket == self.ticket && active.live.load(Ordering::Acquire)
            })
    }
}

impl Drop for Delivery {
    fn drop(&mut self) {
        let mut state = self.intake.0.lock().unwrap();
        if state
            .active
            .get(&self.key)
            .is_some_and(|active| active.ticket == self.ticket)
        {
            state.active.remove(&self.key);
            // The completion pumps already admitted work. Rediscover rows
            // only after explicit invalidation or deferred-capacity pressure;
            // a successful image must not restart every Home rail.
        }
    }
}

/// Explicit close is run-local because static Bridge retains DeviceRuntime.
pub(super) struct RunOwner(pub Arc<Intake>);
impl Drop for RunOwner {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl Bridge {
    /// UI-only identity extraction. Workers receive only captured scalar keys,
    /// owner authority and atomic generation/liveness checks.
    fn intake_slot(
        &self,
        surface: Surface,
        index: usize,
    ) -> Option<(u64, String, String, String, String, String)> {
        let app = self.app()?;
        let state = self.shared.lock().ok()?;
        match surface {
            Surface::Catalog | Surface::HomeCatalog => {
                let preview = if surface == Surface::Catalog {
                    state.previews.get(index)?
                } else {
                    state.home_catalog_row_items.get(index)?
                };
                if surface == Surface::Catalog {
                    let row = app.get_catalog().row_data(index)?;
                    if row.id.as_str() != preview.id
                        || row.poster_path.as_str() != preview.poster.as_deref().unwrap_or_default()
                    {
                        return None;
                    }
                } else {
                    let row = app.get_home_catalog_cards().row_data(index)?;
                    if row.id.as_str() != preview.id
                        || row.media_type.as_str() != preview.type_
                        || row.poster_url.as_str() != preview.poster.as_deref().unwrap_or_default()
                    {
                        return None;
                    }
                }
                let generation = if surface == Surface::Catalog {
                    self.catalog_gen.load(Ordering::Acquire)
                } else {
                    self.home_catalog_rows_gen.load(Ordering::Acquire)
                };
                Some((
                    generation,
                    preview.type_.clone(),
                    preview.id.clone(),
                    preview
                        .extra
                        .get("novaSourceUrl")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    catalog_grouping::display_key(preview),
                    preview.poster.clone().unwrap_or_default(),
                ))
            }
            Surface::Continue | Surface::New => {
                let row = if surface == Surface::Continue {
                    app.get_home_continue()
                } else {
                    app.get_home_new_episodes()
                }
                .row_data(index)?;
                let source = if surface == Surface::Continue {
                    &state.continue_list
                } else {
                    &state.new_episodes_list
                };
                let source = source.iter().find(|source| {
                    source.series_id == row.id.as_str()
                        && (row.media_type.is_empty() || source.type_ == row.media_type.as_str())
                        && (row.episode_id.is_empty()
                            || source.episode_id == row.episode_id.as_str())
                        && state.entries.iter().any(|entry| {
                            entry.type_ == source.type_ && entry.id == source.series_id
                        })
                })?;
                Some((
                    progress_projection::revision(),
                    source.type_.clone(),
                    row.id.to_string(),
                    "library".into(),
                    source.episode_id.clone(),
                    row.art_url.to_string(),
                ))
            }
            Surface::Upcoming => {
                let row = app.get_home_upcoming().row_data(index)?;
                let source = state.upcoming_list.get(row.index as usize)?;
                if source.series_id != row.id.as_str() {
                    return None;
                }
                Some((
                    progress_projection::revision(),
                    source.type_.clone(),
                    row.id.to_string(),
                    "library".into(),
                    source.episode_id.clone(),
                    row.art_url.to_string(),
                ))
            }
            Surface::History => {
                let row = app.get_home_history_archive_cards().row_data(index)?;
                Some((
                    progress_projection::revision(),
                    row.media_type.to_string(),
                    row.id.to_string(),
                    "history".into(),
                    row.flat_index.to_string(),
                    row.poster_url.to_string(),
                ))
            }
        }
    }
    fn intake_key_matches(&self, key: &Key) -> bool {
        let current = profiles::active_context();
        (!profiles::enabled() || (current.id == key.profile && current.epoch == key.epoch))
            && key.authority.1 == image_settings_revision()
            && self.intake_slot(key.surface, key.index).is_some_and(
                |(generation, media_type, id, source, projection, url)| {
                    generation == key.generation
                        && media_type == key.media_type
                        && id == key.id
                        && source == key.source
                        && projection == key.projection
                        && url == key.url
                },
            )
    }
    fn intake_slot_matches(&self, request: &Request) -> bool {
        profiles::is_current(&request.context)
            && !request
                .context
                .session
                .as_ref()
                .is_some_and(|session| session.cancelled.load(Ordering::Acquire))
            && self.intake_key_matches(&request.key)
    }
    fn intake_matches(&self, request: &Request) -> bool {
        self.intake_slot_matches(request) && request.owner.is_current()
    }

    // Test fixtures can exercise individual admissions. Controllers retain
    // one lazy batch for a complete synchronous model/viewport dispatch.
    #[cfg(test)]
    pub(super) fn enqueue_poster_intake(
        &self,
        surface: Surface,
        index: usize,
        visible: bool,
        fallback: String,
    ) -> Admission {
        let admission = self.queue_poster_intake(surface, index, visible, fallback);
        self.pump_poster_intake();
        admission
    }

    #[cfg(test)]
    pub(super) fn queue_poster_intake(
        &self,
        surface: Surface,
        index: usize,
        visible: bool,
        fallback: String,
    ) -> Admission {
        self.queue_poster_batch(&Batch::default(), surface, index, visible, fallback)
    }

    fn capture_poster_batch(&self, context: &profiles::Context) -> Option<OwnedBatch> {
        let _timing = perf::Timer::new("poster.owner_capture");
        #[cfg(test)]
        OWNER_CAPTURES.fetch_add(1, Ordering::Relaxed);
        let (settings, inventory) = if profiles::enabled() {
            let session = context.session.as_ref()?;
            let owner = session.providers.get()?;
            let settings = owner.with(nova_providers::plugin_settings::capture_settings_capability);
            let inventory = session.provider_inventory().ok()?;
            (settings, inventory)
        } else {
            (
                nova_providers::plugin_settings::capture_settings_capability(),
                nova_providers::OwnedProviderSnapshot::capture(),
            )
        };
        let owner = ImageFetchOwner::new(settings, inventory);
        owner.is_current().then(|| OwnedBatch {
            context: context.clone(),
            owner,
        })
    }

    /// Capture source authority once per dispatch, after empty/stale slots are
    /// rejected. A batch is local to the caller and never survives navigation.
    pub(super) fn queue_poster_batch(
        &self,
        batch: &Batch,
        surface: Surface,
        index: usize,
        visible: bool,
        fallback: String,
    ) -> Admission {
        if self.poster_intake.0.lock().unwrap().closed {
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
        let Some((generation, media_type, id, source, projection, url)) =
            self.intake_slot(surface, index)
        else {
            return Admission::Obsolete;
        };
        if url.is_empty() {
            return Admission::Obsolete;
        }
        let Some(captured) = batch.0.get_or_init(|| self.capture_poster_batch(&context)) else {
            return Admission::Obsolete;
        };
        if captured.context.id != context.id
            || captured.context.epoch != context.epoch
            || !profiles::is_current(&captured.context)
            || !captured.owner.is_current()
        {
            return Admission::Obsolete;
        }
        let context = captured.context.clone();
        let owner = captured.owner.clone();
        let key = Key {
            profile: context.id.clone(),
            epoch: context.epoch,
            surface,
            generation,
            index,
            media_type,
            id,
            source,
            projection,
            recipe: image_cache_key(&url, ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE))),
            url,
            fallback,
            authority: owner.generation(),
        };
        self.poster_intake.submit(Request {
            key,
            context,
            owner,
            visible,
            fallback_attempt: false,
        })
    }
    /// Idle ticks observe the image-settings revision; shared artwork
    /// availability is read only while work is pending. Settled models are
    /// scanned again only after an actual recipe change.
    pub(super) fn poster_intake_tick(&self) {
        if self
            .poster_intake
            .needs_pump(nova_media::net::artwork_availability_revision)
        {
            self.pump_poster_intake();
        }
    }

    pub(super) fn refresh_poster_intake(&self) {
        self.poster_intake.0.lock().unwrap().refresh = true;
        self.pump_poster_intake();
    }
    pub(super) fn pump_poster_intake(&self) {
        if !self
            .poster_intake
            .begin_pump(nova_media::net::artwork_availability_revision)
        {
            return;
        }
        let (refresh, recipe_changed) = {
            let mut state = self.poster_intake.0.lock().unwrap();
            let revision = image_settings_revision();
            let recipe_changed = state
                .recipe_revision
                .replace(revision)
                .is_some_and(|previous| previous != revision);
            let removed = state.high.len() + state.low.len();
            state.high.retain(|request| self.intake_matches(request));
            state.low.retain(|request| self.intake_matches(request));
            for (key, active) in &mut state.active {
                if !self.intake_key_matches(key) {
                    active.live.store(false, Ordering::Release);
                    if let Some(handle) = active.handle.take() {
                        handle.cancel();
                    }
                }
            }

            let refresh = state.refresh
                || state.deferred
                || recipe_changed
                || removed != state.high.len() + state.low.len();
            state.refresh = false;
            state.deferred = false;
            (refresh, recipe_changed)
        };
        if recipe_changed {
            self.invalidate_home_poster_recipes();
        }
        if refresh && let Some(app) = self.app() {
            if app.get_show_home() && !app.get_profiles_require_selection() {
                self.dispatch_continue_posters();
                self.dispatch_upcoming_posters();
                self.dispatch_home_catalog_posters();
            }
            self.queue_visible_catalog_posters();
        }
        loop {
            let next = {
                let mut state = self.poster_intake.0.lock().unwrap();
                if state.closed || state.active.len() >= DELIVERIES {
                    None
                } else {
                    state
                        .high
                        .pop_front()
                        .or_else(|| state.low.pop_front())
                        .map(|request| {
                            state.ticket = state.ticket.wrapping_add(1);
                            let ticket = state.ticket;
                            let live = Arc::new(AtomicBool::new(true));
                            state.active.insert(
                                request.key.clone(),
                                Active {
                                    ticket,
                                    live: live.clone(),
                                    handle: None,
                                },
                            );
                            (request, ticket, live)
                        })
                }
            };
            let Some((request, ticket, live)) = next else {
                break;
            };
            let key = request.key.clone();
            let before_attempt = nova_media::net::artwork_availability_revision();
            let delivery = Delivery {
                intake: self.poster_intake.clone(),
                key: key.clone(),
                ticket,
            };
            let bridge = self.clone();
            let context = request.context.clone();
            let counter = if key.surface == Surface::Catalog {
                Some(self.catalog_gen.clone())
            } else if key.surface == Surface::HomeCatalog {
                Some(self.home_catalog_rows_gen.clone())
            } else {
                None
            };
            let generation = key.generation;
            let image_use = ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE));
            let url = if request.fallback_attempt {
                key.fallback.clone()
            } else {
                key.url.clone()
            };
            let handle = nova_media::net::fetch_image_for_owner_result_controlled(
                url,
                image_use,
                request.owner.clone(),
                Arc::new(move || {
                    live.load(Ordering::Acquire)
                        && profiles::is_current(&context)
                        && !context
                            .session
                            .as_ref()
                            .is_some_and(|session| session.cancelled.load(Ordering::Acquire))
                        && counter.as_ref().map_or_else(
                            || progress_projection::revision() == generation,
                            |counter| counter.load(Ordering::Acquire) == generation,
                        )
                }),
                move |result| {
                    // Cleanup runs even when the originating profile is gone.
                    let _ = profiles::invoke_account(move || {
                        bridge.complete_poster_delivery(request, delivery, before_attempt, result);
                    });
                },
            );
            let mut state = self.poster_intake.0.lock().unwrap();
            if let Some(active) = state
                .active
                .get_mut(&key)
                .filter(|active| active.ticket == ticket)
            {
                active.handle = Some(handle);
            } else {
                handle.cancel();
            }
        }
        self.poster_intake.0.lock().unwrap().pumping = false;
    }
    /// UI terminal delivery. Registration liveness, exact slot identity and
    /// captured authority are independent checks, including after a queue delay.
    fn complete_poster_delivery(
        &self,
        request: Request,
        delivery: Delivery,
        before_attempt: u64,
        result: Result<SharedPixelBuffer<Rgba8Pixel>, ImageFetchFailure>,
    ) {
        let slot_current = self.intake_slot_matches(&request);
        let authority_current = request.owner.is_current();
        let live = delivery.is_live();
        let current = live && slot_current && authority_current;
        drop(delivery);
        // Authority can expire while a job or UI delivery waits. Rediscover the
        // current viewport with fresh authority; never requeue the obsolete owner.
        if live && slot_current && !authority_current && request.key.surface == Surface::Catalog {
            self.poster_intake.0.lock().unwrap().refresh = true;
        }
        if current {
            match result {
                Err(ImageFetchFailure::Busy) => {
                    self.poster_intake.submit(request);
                    self.poster_intake.0.lock().unwrap().busy_revision = Some(before_attempt);
                    return;
                }
                Err(ImageFetchFailure::SourceFailure)
                    if !request.fallback_attempt
                        && !request.key.fallback.is_empty()
                        && request.key.fallback != request.key.url =>
                {
                    let mut request = request;
                    request.fallback_attempt = true;
                    self.poster_intake.submit(request);
                }
                Ok(pixels) => self.publish_intake_poster(&request.key, pixels),
                Err(ImageFetchFailure::Cancelled) => {}
                Err(_) => {
                    {
                        let mut state = self.poster_intake.0.lock().unwrap();
                        if state.failures.len() == VISIBLE + BACKGROUND {
                            state.failures.pop_front();
                        }
                        state
                            .failures
                            .push_back((request.key.clone(), Instant::now()));
                    }
                    profiles::with_context(request.context.clone(), || {
                        if request.key.surface == Surface::Catalog {
                            self.recover_catalog_artwork(
                                request.key.generation,
                                request.key.index,
                                &request.key.url,
                            );
                        } else {
                            self.recover_missing_artwork(
                                &request.key.media_type,
                                &request.key.id,
                                if request.key.fallback.is_empty() {
                                    &request.key.url
                                } else {
                                    &request.key.fallback
                                },
                            );
                        }
                    });
                }
            }
        }
        profiles::with_context(profiles::active_context(), || self.pump_poster_intake());
    }

    fn publish_intake_poster(&self, key: &Key, pixels: SharedPixelBuffer<Rgba8Pixel>) {
        let Some(app) = self.app() else {
            return;
        };
        match key.surface {
            Surface::Catalog => {
                app.invoke_set_card_poster(key.index as i32, Image::from_rgba8(pixels))
            }
            Surface::HomeCatalog | Surface::History => {
                self.publish_discover_poster(&key.media_type, &key.id, &key.url, &pixels)
            }
            Surface::Continue | Surface::New => {
                let model = if key.surface == Surface::Continue {
                    app.get_home_continue()
                } else {
                    app.get_home_new_episodes()
                };
                if let Some(mut row) = model.row_data(key.index) {
                    row.poster = Image::from_rgba8(pixels);
                    row.is_loaded = true;
                    model.set_row_data(key.index, row);
                }
            }
            Surface::Upcoming => {
                let image = Image::from_rgba8(pixels);
                let model = app.get_home_upcoming();
                if let Some(mut row) = model.row_data(key.index) {
                    row.poster = image.clone();
                    row.is_loaded = true;
                    model.set_row_data(key.index, row);
                }
                let model = app.get_home_cal_day();
                let state = self.shared.lock().unwrap();
                for (index, mut row) in model
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| row.id == key.id && row.art_url == key.url)
                {
                    if state
                        .upcoming_list
                        .get(row.index as usize)
                        .is_some_and(|source| {
                            source.type_ == key.media_type && source.episode_id == key.projection
                        })
                    {
                        row.poster = image.clone();
                        row.is_loaded = true;
                        model.set_row_data(index, row);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "desktop")]
    #[test]
    fn home_busy_retries_after_unrelated_artwork_releases_capacity_without_scroll() {
        use nova_media::net::ARTWORK_PENDING_CAPACITY;
        use std::io::{Read, Write};
        const CHILD: &str = "NOVA_HOME_BUSY_INTAKE_CHILD";
        let Some(root) = std::env::var_os(CHILD) else {
            let root = std::env::temp_dir().join(format!(
                "nova-home-busy-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(CHILD, &root)
                .env("XDG_CACHE_HOME", root.join("cache"))
                .args(["--exact", "app::poster_intake::tests::home_busy_retries_after_unrelated_artwork_releases_capacity_without_scroll", "--nocapture"])
                .output().unwrap();
            let _ = fs::remove_dir_all(&root);
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
        app.set_show_home(true);
        app.set_profiles_require_selection(false);
        let bridge = Bridge::new(
            app.as_weak(),
            Arc::new(AtomicU64::new(0)),
            crate::player::Player::setup(&app),
            DownloadCoordinator::new(root.join("downloads")),
        );
        let owner = RunOwner(bridge.poster_intake.clone());

        // Occupy every REAL media artwork worker, not the app delivery leases.
        // The listener stays open to detect any unintended target HTTP fetch.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server_listener = listener.try_clone().unwrap();
        let (started, observed) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut streams = Vec::new();
            for _ in 0..DELIVERIES {
                let mut stream = loop {
                    match server_listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "artwork worker never connected");
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("fixture accept: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(8)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(8)))
                    .unwrap();
                let mut request = [0; 2048];
                assert!(stream.read(&mut request).unwrap() > 0);
                streams.push(stream);
                started.send(()).unwrap();
            }
            released.recv_timeout(Duration::from_secs(12)).unwrap();
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgba8(2, 3)
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            let body = png.into_inner();
            for mut stream in streams {
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        let finished = Arc::new(AtomicUsize::new(0));
        for index in 0..DELIVERIES {
            let finished = finished.clone();
            nova_media::net::fetch_image_for_owner_result_controlled(
                format!("{base}/blocked-{index}.png"),
                ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
                ImageFetchOwner::capture(),
                Arc::new(|| true),
                move |result| {
                    assert!(result.is_ok());
                    finished.fetch_add(1, Ordering::SeqCst);
                },
            );
        }
        for _ in 0..DELIVERIES {
            observed.recv_timeout(Duration::from_secs(8)).unwrap();
        }
        // All pending slots belong to other consumers. Cancelled jobs still
        // occupy a queued slot until a worker pops and terminally completes them.
        for index in 0..ARTWORK_PENDING_CAPACITY {
            let finished = finished.clone();
            let handle = nova_media::net::fetch_image_for_owner_result_controlled(
                format!("{base}/cancelled-{index}.png"),
                ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
                ImageFetchOwner::capture(),
                Arc::new(|| false),
                move |result| {
                    assert_eq!(result.err(), Some(ImageFetchFailure::Cancelled));
                    finished.fetch_add(1, Ordering::SeqCst);
                },
            );
            handle.cancel();
        }
        assert_eq!(
            finished.load(Ordering::SeqCst),
            0,
            "all artwork workers remain behind the HTTP barrier"
        );
        let target = format!("{base}/target.png");
        decoded_cache_insert(
            &image_cache_key(&target, ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE))),
            SharedPixelBuffer::new(2, 3),
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            state.entries = vec![
                serde_json::from_value::<LibraryEntry>(serde_json::json!({
                    "id":"target", "type_":"series", "name":"Target", "poster_url":target
                }))
                .unwrap(),
            ];
            state.continue_list = vec![ContinueEntry {
                series_id: "target".into(),
                type_: "series".into(),
                episode_id: "target:1:1".into(),
                ..Default::default()
            }];
        }
        app.set_home_continue(
            Rc::new(VecModel::from(vec![ContinueRow {
                id: "target".into(),
                media_type: "series".into(),
                episode_id: "target:1:1".into(),
                art_url: target.into(),
                ..Default::default()
            }]))
            .into(),
        );
        bridge.dispatch_continue_posters();

        let saw_busy = Rc::new(std::cell::Cell::new(false));
        let passed = Rc::new(std::cell::Cell::new(false));
        let saw_busy_tick = saw_busy.clone();
        let passed_tick = passed.clone();
        let mut release = Some(release);
        let started = Instant::now();
        let weak = app.as_weak();
        let tick_bridge = bridge.clone();
        let finished_tick = finished.clone();
        let timer = slint::Timer::default();
        // This is the actual existing tick entrypoint and cadence. No scroll,
        // direct scheduler pump or manual image publication drives progress.
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(250),
            move || {
                let app = weak.upgrade().unwrap();
                {
                    let state = tick_bridge.poster_intake.0.lock().unwrap();
                    assert!(state.active.len() <= DELIVERIES);
                    if !saw_busy_tick.get() && state.busy_revision.is_some() {
                        assert_eq!(state.high.len(), 1, "Busy must retain the current target");
                        assert!(
                            state.active.is_empty(),
                            "Busy payload lease is acknowledged on UI"
                        );
                        assert!(
                            state.failures.is_empty(),
                            "Busy cannot charge source cooldown"
                        );
                        assert!(!app.get_home_continue().row_data(0).unwrap().is_loaded);
                        saw_busy_tick.set(true);
                    }
                }
                if saw_busy_tick.get()
                    && let Some(release) = release.take()
                {
                    // No availability change yet: the same production tick must
                    // leave the deferred target pending and avoid another attempt.
                    tick_bridge.poster_intake_tick();
                    assert!(
                        tick_bridge
                            .poster_intake
                            .0
                            .lock()
                            .unwrap()
                            .busy_revision
                            .is_some()
                    );
                    release.send(()).unwrap();
                }
                tick_bridge.poster_intake_tick();
                let loaded = app.get_home_continue().row_data(0).unwrap().is_loaded;
                let settled = {
                    let state = tick_bridge.poster_intake.0.lock().unwrap();
                    assert!(state.failures.is_empty());
                    state.active.is_empty() && state.high.is_empty() && state.low.is_empty()
                };
                if saw_busy_tick.get()
                    && loaded
                    && settled
                    && finished_tick.load(Ordering::SeqCst) == DELIVERIES + ARTWORK_PENDING_CAPACITY
                {
                    passed_tick.set(true);
                    slint::quit_event_loop().unwrap();
                } else if started.elapsed() > Duration::from_secs(8) {
                    if let Some(release) = release.take() {
                        let _ = release.send(());
                    }
                    slint::quit_event_loop().unwrap();
                }
            },
        );
        app.run().unwrap();
        timer.stop();
        server.join().unwrap();
        assert!(
            saw_busy.get(),
            "fixture must exercise actual typed Busy admission"
        );
        assert!(
            passed.get(),
            "availability revision must retry stationary Home after unrelated work releases capacity"
        );
        assert_eq!(
            finished.load(Ordering::SeqCst),
            DELIVERIES + ARTWORK_PENDING_CAPACITY,
            "every unrelated request completes once"
        );
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "cached target and cancelled work must not issue additional HTTP"
        );
        let row = app.get_home_continue().row_data(0).unwrap();
        assert!(row.is_loaded);
        assert_eq!(row.poster.size().width, 2);
        assert_eq!(row.poster.size().height, 3);
        {
            let state = bridge.shared.lock().unwrap();
            assert!(
                state.artwork_recovery.is_empty(),
                "Busy must not launch source recovery"
            );
            assert!(state.pending_artwork_recovery.is_empty());
            assert!(state.metadata_prefetch.is_empty());
        }
        drop(owner);
        let state = bridge.poster_intake.0.lock().unwrap();
        assert!(
            state.closed
                && state.active.is_empty()
                && state.high.is_empty()
                && state.low.is_empty()
        );
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn home_fallbacks_settle_without_lru_and_idle_recipe_change_refreshes_once() {
        use std::io::{Read, Write};
        const CHILD: &str = "NOVA_HOME_SETTLED_INTAKE_CHILD";
        let Some(root) = std::env::var_os(CHILD) else {
            let root = std::env::temp_dir().join(format!(
                "nova-home-settled-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(CHILD, &root)
                .env("XDG_CACHE_HOME", root.join("cache"))
                .args(["--exact", "app::poster_intake::tests::home_fallbacks_settle_without_lru_and_idle_recipe_change_refreshes_once", "--nocapture"])
                .output().unwrap();
            let _ = fs::remove_dir_all(&root);
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
            enabled: false,
            lru_cache_mb: 0,
            ..Default::default()
        });
        i_slint_backend_testing::init_integration_test_with_system_time();
        let app = AppWindow::new().unwrap();
        app.set_show_home(true);
        app.set_profiles_require_selection(false);
        let bridge = Bridge::new(
            app.as_weak(),
            Arc::new(AtomicU64::new(0)),
            crate::player::Player::setup(&app),
            DownloadCoordinator::new(root.join("downloads")),
        );
        let _owner = RunOwner(bridge.poster_intake.clone());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let counts = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let server_counts = counts.clone();
        let server_stop = stopped.clone();
        let server = std::thread::spawn(move || {
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgba8(2, 3)
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            let png = png.into_inner();
            while !server_stop.load(Ordering::Acquire) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = [0; 2048];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                let path = request.split_whitespace().nth(1).unwrap().to_owned();
                *server_counts
                    .lock()
                    .unwrap()
                    .entry(path.clone())
                    .or_default() += 1;
                let (status, body) = if path.contains("bad") {
                    ("404 Not Found", &[][..])
                } else {
                    ("200 OK", png.as_slice())
                };
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(body).unwrap();
            }
        });
        {
            let mut state = bridge.shared.lock().unwrap();
            state.entries = (0..3).map(|index| serde_json::from_value(serde_json::json!({
                "id":format!("rail-{index}"), "type_":"series", "name":"Rail", "poster_url":format!("{base}/good-{index}.png")
            })).unwrap()).collect();
            let source = |index| ContinueEntry {
                series_id: format!("rail-{index}"),
                type_: "series".into(),
                episode_id: format!("episode-{index}"),
                ..Default::default()
            };
            state.continue_list = vec![source(0)];
            state.new_episodes_list = vec![source(1)];
            state.upcoming_list = vec![UpcomingEntry {
                series_id: "rail-2".into(),
                type_: "series".into(),
                episode_id: "episode-2".into(),
                ..Default::default()
            }];
            state.home_catalog_row_items = vec![MetaPreview {
                id: "catalog".into(),
                type_: "series".into(),
                poster: Some(format!("{base}/catalog.png")),
                ..Default::default()
            }];
        }
        // Catalog publication merges its metadata header. Seed that exact
        // existing snapshot before admitting rail work so this fixture tests
        // successful delivery, rather than an unrelated progress invalidation.
        write_meta_header_for(
            "series",
            "catalog",
            &MetaHeader {
                poster_url: format!("{base}/catalog.png"),
                ..Default::default()
            },
        );
        let projection_revision = progress_projection::revision();
        let row = |index| ContinueRow {
            id: format!("rail-{index}").into(),
            media_type: "series".into(),
            episode_id: format!("episode-{index}").into(),
            art_url: format!("{base}/bad-{index}.png").into(),
            ..Default::default()
        };
        app.set_home_continue(Rc::new(VecModel::from(vec![row(0)])).into());
        app.set_home_new_episodes(Rc::new(VecModel::from(vec![row(1)])).into());
        let upcoming = UpcomingRow {
            id: "rail-2".into(),
            art_url: format!("{base}/bad-2.png").into(),
            index: 0,
            ..Default::default()
        };
        app.set_home_upcoming(Rc::new(VecModel::from(vec![upcoming.clone()])).into());
        app.set_home_cal_day(Rc::new(VecModel::from(vec![upcoming])).into());
        app.set_home_catalog_cards(
            Rc::new(VecModel::from(vec![HomeCatalogCard {
                id: "catalog".into(),
                media_type: "series".into(),
                poster_url: format!("{base}/catalog.png").into(),
                ..Default::default()
            }]))
            .into(),
        );
        OWNER_CAPTURES.store(0, Ordering::Relaxed);
        bridge.dispatch_continue_posters();
        bridge.dispatch_upcoming_posters();
        bridge.dispatch_home_catalog_posters();

        let passed = Rc::new(std::cell::Cell::new(false));
        let passed_tick = passed.clone();
        let tick_bridge = bridge.clone();
        let weak = app.as_weak();
        let started = Instant::now();
        let tick_counts = counts.clone();
        let recipe_url = format!("{base}/bad-0.png");
        let initial_recipe =
            image_cache_key(&recipe_url, ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)));
        let initial_image_revision = image_settings_revision();
        let mut quiet_ticks = 0;
        let mut revised = false;
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(10),
            move || {
                let app = weak.upgrade().unwrap();
                assert_eq!(
                    progress_projection::revision(),
                    projection_revision,
                    "successful image publication must not edit the preseeded metadata snapshot"
                );
                tick_bridge.poster_intake_tick();
                let loaded = app.get_home_continue().iter().all(|row| row.is_loaded)
                    && app.get_home_new_episodes().iter().all(|row| row.is_loaded)
                    && app.get_home_upcoming().iter().all(|row| row.is_loaded)
                    && app.get_home_cal_day().iter().all(|row| row.is_loaded)
                    && app.get_home_catalog_cards().iter().all(|row| row.is_loaded);
                let settled = {
                    let state = tick_bridge.poster_intake.0.lock().unwrap();
                    assert!(state.active.len() <= DELIVERIES);
                    state.active.is_empty()
                        && state.high.is_empty()
                        && state.low.is_empty()
                        && !state.refresh
                        && !state.deferred
                        && state.busy_revision.is_none()
                };
                if loaded && settled {
                    assert_eq!(
                        decoded_cache_stats().0,
                        0,
                        "row images survive without LRU residency"
                    );
                    // Exercise explicit redispatch too: success/fallback must not
                    // become pending just because its original URL is uncached.
                    tick_bridge.dispatch_continue_posters();
                    tick_bridge.dispatch_upcoming_posters();
                    tick_bridge.dispatch_home_catalog_posters();
                    assert_eq!(
                        OWNER_CAPTURES.load(Ordering::Relaxed),
                        if revised { 6 } else { 3 },
                        "one lazy authority snapshot per active dispatch batch; settled/loaded redispatches capture none"
                    );
                    let counts = tick_counts.lock().unwrap();
                    assert_eq!(counts.len(), 7);
                    assert!(
                        counts
                            .values()
                            .all(|count| *count == if revised { 2 } else { 1 }),
                        "unexpected repeated fetches (revised={revised}): {counts:?}"
                    );
                    drop(counts);
                    quiet_ticks += 1;
                    if quiet_ticks == 12 {
                        if revised {
                            passed_tick.set(true);
                            slint::quit_event_loop().unwrap();
                        } else {
                            let mut settings = active_cache_settings();
                            settings.enabled = true;
                            set_active_cache_settings(settings);
                            assert_ne!(image_settings_revision(), initial_image_revision);
                            assert_ne!(
                                image_cache_key(
                                    &recipe_url,
                                    ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE))
                                ),
                                initial_recipe,
                                "the second wave must use an actually different resolved recipe"
                            );
                            // No navigation/invalidation callback: the existing
                            // idle tick must notice this actual recipe revision.
                            assert!(
                                tick_bridge
                                    .poster_intake
                                    .needs_pump(nova_media::net::artwork_availability_revision)
                            );
                            tick_bridge.poster_intake_tick();
                            assert!(!app.get_home_continue().row_data(0).unwrap().is_loaded);
                            assert!(!app.get_home_upcoming().row_data(0).unwrap().is_loaded);
                            assert!(!app.get_home_catalog_cards().row_data(0).unwrap().is_loaded);
                            revised = true;
                            quiet_ticks = 0;
                        }
                    }
                } else {
                    quiet_ticks = 0;
                }
                if started.elapsed() > Duration::from_secs(8) {
                    slint::quit_event_loop().unwrap();
                }
            },
        );
        app.run().unwrap();
        timer.stop();
        stopped.store(true, Ordering::Release);
        server.join().unwrap();
        assert!(
            passed.get(),
            "Home must settle before and after one recipe refresh"
        );
        assert!(
            bridge.poster_intake.0.lock().unwrap().failures.is_empty(),
            "successful fallback never charges a terminal failure"
        );
        let size = app.get_home_continue().row_data(0).unwrap().poster.size();
        assert_eq!((size.width, size.height), (2, 3));
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn catalog_capacity_retries_stationary_viewport_and_discover_return() {
        const CHILD: &str = "NOVA_SHARED_POSTER_VIEWPORT_CHILD";
        let Some(root) = std::env::var_os(CHILD) else {
            let root = std::env::temp_dir().join(format!(
                "nova-poster-viewport-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(CHILD, &root)
                .env("XDG_CACHE_HOME", root.join("cache"))
                .args(["--exact", "app::poster_intake::tests::catalog_capacity_retries_stationary_viewport_and_discover_return", "--nocapture"])
                .output().unwrap();
            let _ = fs::remove_dir_all(&root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };

        // Exercise the real shared media executor and terminal Slint deliveries.
        fn pump_until(app: &AppWindow, bridge: &Bridge, ready: impl Fn() -> bool + 'static) {
            let started = std::time::Instant::now();
            let timer = Rc::new(slint::Timer::default());
            let tick_bridge = bridge.clone();
            let stop = Rc::downgrade(&timer);
            let outcome = Rc::new(std::cell::Cell::new(None));
            let observed = outcome.clone();
            timer.start(
                slint::TimerMode::Repeated,
                Duration::from_millis(5),
                move || {
                    // The testing backend updates timers before dispatching
                    // even an already-queued Quit. Post exactly one Quit so
                    // another pump cannot inherit a duplicate exit request.
                    if observed.get().is_some() {
                        return;
                    }
                    tick_bridge.poster_intake_tick();
                    let satisfied = ready();
                    if satisfied || started.elapsed() > Duration::from_secs(8) {
                        observed.set(Some(satisfied));
                        if let Some(timer) = stop.upgrade() {
                            timer.stop();
                        }
                        slint::quit_event_loop().unwrap();
                    }
                },
            );
            // ComponentHandle::run hides its window after returning, which
            // queues another last-window Quit in the testing backend. Keep
            // the same window shown across this fixture's separate pumps.
            app.show().unwrap();
            slint::run_event_loop().unwrap();
            timer.stop();
            assert_eq!(
                outcome.get(),
                Some(true),
                "event loop must reach its current predicate before the deadline"
            );
        }
        fn blocked_image() -> (
            String,
            mpsc::Receiver<()>,
            mpsc::Sender<()>,
            std::thread::JoinHandle<()>,
        ) {
            use std::io::{Read, Write};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}/blocked.png", listener.local_addr().unwrap());
            let (started, observed) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let server = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(8);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "poster worker never connected"
                            );
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("fixture accept: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(8)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(8)))
                    .unwrap();
                let mut request = [0; 2048];
                assert!(stream.read(&mut request).unwrap() > 0);
                started.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(8)).unwrap();
                let mut png = std::io::Cursor::new(Vec::new());
                image::DynamicImage::new_rgba8(2, 3)
                    .write_to(&mut png, image::ImageFormat::Png)
                    .unwrap();
                let body = png.into_inner();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            });
            (url, observed, release, server)
        }
        fn install_rows(
            bridge: &Bridge,
            app: &AppWindow,
            generation: u64,
            blockers: [&str; DELIVERIES],
        ) -> String {
            let target = VISIBLE + DELIVERIES;
            let previews = (0..=target)
                .map(|index| {
                    let url = if index < DELIVERIES {
                        blockers[index].to_owned()
                    } else {
                        format!("https://fixture.invalid/{generation}/{index}.png")
                    };
                    if index >= DELIVERIES {
                        decoded_cache_insert(
                            &image_cache_key(&url, ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE))),
                            SharedPixelBuffer::new(2, 3),
                        );
                    }
                    serde_json::from_value::<MetaPreview>(serde_json::json!({
                    "id": format!("row-{index}"), "type": "movie", "name": format!("Row {index}"),
                    "poster": url, "novaSourceUrl": "https://fixture.invalid/source"
                })).unwrap()
                })
                .collect::<Vec<_>>();
            let target_url = previews[target].poster.clone().unwrap();
            app.set_catalog(
                Rc::new(VecModel::from(
                    previews
                        .iter()
                        .map(|preview| MediaCard {
                            id: preview.id.clone().into(),
                            poster_path: preview.poster.clone().unwrap().into(),
                            ..Default::default()
                        })
                        .collect::<Vec<_>>(),
                ))
                .into(),
            );
            {
                let mut state = bridge.shared.lock().unwrap();
                state.previews = previews;
            }
            target_url
        }
        fn settled(bridge: &Bridge) -> bool {
            let state = bridge.poster_intake.0.lock().unwrap();
            assert!(state.active.len() <= DELIVERIES);
            state.active.is_empty() && state.high.is_empty() && state.low.is_empty()
        }
        fn fill_while_blocked(bridge: &Bridge) {
            for index in DELIVERIES..VISIBLE + DELIVERIES {
                assert_eq!(
                    bridge.queue_poster_intake(Surface::Catalog, index, true, String::new()),
                    Admission::Queued
                );
            }
            assert_eq!(bridge.poster_intake.0.lock().unwrap().high.len(), VISIBLE);
            // Report the viewport once. Capacity release must retry it without
            // another scroll, including after returning from a hidden Discover.
            bridge.visible_range(
                false,
                (VISIBLE + DELIVERIES) as i32,
                (VISIBLE + DELIVERIES) as i32,
            );
            bridge.refresh_poster_intake();
            assert!(bridge.poster_intake.0.lock().unwrap().deferred);
        }

        let root = PathBuf::from(root);
        storage::init_at(&root);
        profiles::initialize().unwrap();
        set_active_cache_settings(CacheSettings {
            cache_images: false,
            ..Default::default()
        });
        i_slint_backend_testing::init_integration_test_with_system_time();
        let app = AppWindow::new().unwrap();
        app.set_show_home(false);
        app.set_more_open(false);
        app.set_profiles_require_selection(false);
        let generation = Arc::new(AtomicU64::new(1));
        let bridge = Bridge::new(
            app.as_weak(),
            generation.clone(),
            crate::player::Player::setup(&app),
            DownloadCoordinator::new(root.join("downloads")),
        );
        let owner = RunOwner(bridge.poster_intake.clone());
        let target = VISIBLE + DELIVERIES;

        for hidden in [false, true] {
            // Initial invalidation must not pre-admit the default viewport
            // while the fixture is establishing the blocked physical leases.
            app.set_show_home(true);
            let current = generation.fetch_add(1, Ordering::AcqRel) + 1;
            bridge.poster_intake.invalidate();
            let blockers: [_; DELIVERIES] = std::array::from_fn(|_| blocked_image());
            let urls = std::array::from_fn(|index| blockers[index].0.as_str());
            install_rows(&bridge, &app, current, urls);
            for index in 0..DELIVERIES {
                assert_eq!(
                    bridge.enqueue_poster_intake(Surface::Catalog, index, true, String::new()),
                    Admission::Queued
                );
            }
            for (_, observed, _, _) in &blockers {
                observed.recv_timeout(Duration::from_secs(8)).unwrap();
            }
            app.set_show_home(false);
            fill_while_blocked(&bridge);
            assert!(!app.get_catalog().row_data(target).unwrap().is_loaded);
            profiles::with_context(profiles::context_for("foreign-owner"), || {
                assert_eq!(
                    bridge.queue_poster_intake(Surface::Catalog, target, true, String::new()),
                    Admission::Obsolete
                );
            });
            app.set_show_home(hidden);
            for (_, _, release, _) in &blockers {
                release.send(()).unwrap();
            }
            for (_, _, _, server) in blockers {
                server.join().unwrap();
            }
            let weak = app.as_weak();
            let observed_bridge = bridge.clone();
            pump_until(&app, &bridge, move || {
                settled(&observed_bridge)
                    && (hidden
                        || weak
                            .upgrade()
                            .unwrap()
                            .get_catalog()
                            .row_data(target)
                            .unwrap()
                            .is_loaded)
            });
            if hidden {
                assert!(
                    !app.get_catalog().row_data(target).unwrap().is_loaded,
                    "capacity release cannot repopulate hidden Discover"
                );
                bridge.show_discover_page();
                let weak = app.as_weak();
                pump_until(&app, &bridge, move || {
                    weak.upgrade()
                        .unwrap()
                        .get_catalog()
                        .row_data(target)
                        .unwrap()
                        .is_loaded
                });
            }
            assert!(
                app.get_catalog().row_data(target).unwrap().is_loaded,
                "current viewport eventually paints without another scroll"
            );
        }

        // Running obsolete work cannot paint an identical slot/URL after its
        // catalog generation changes, and cancellation keeps its physical lease.
        app.set_show_home(true);
        let current = generation.fetch_add(1, Ordering::AcqRel) + 1;
        bridge.poster_intake.invalidate();
        let (url, observed, release, server) = blocked_image();
        install_rows(&bridge, &app, current, [url.as_str(); DELIVERIES]);
        assert_eq!(
            bridge.enqueue_poster_intake(Surface::Catalog, 0, true, String::new()),
            Admission::Queued
        );
        observed.recv_timeout(Duration::from_secs(8)).unwrap();
        generation.fetch_add(1, Ordering::AcqRel);
        bridge.poster_intake.invalidate();
        assert_eq!(bridge.poster_intake.0.lock().unwrap().active.len(), 1);
        app.set_show_home(true);
        release.send(()).unwrap();
        server.join().unwrap();
        let observed_bridge = bridge.clone();
        pump_until(&app, &bridge, move || settled(&observed_bridge));
        assert!(
            !app.get_catalog().row_data(0).unwrap().is_loaded,
            "obsolete running work cannot paint"
        );
        drop(owner);
        assert!(
            bridge.poster_intake.0.lock().unwrap().closed,
            "run-local close survives retained Bridge handles"
        );
    }

    fn prepare_catalog_delivery(bridge: &Bridge, ticket: u64) -> (Request, Delivery) {
        let app = bridge.app().unwrap();
        let url = "https://fixture.invalid/catalog-delivery.png";
        bridge.shared.lock().unwrap().previews = vec![
            serde_json::from_value::<MetaPreview>(serde_json::json!({
                "id":"delivery", "type":"movie", "name":"Delivery", "poster":url,
                "novaSourceUrl":"https://fixture.invalid/source"
            }))
            .unwrap(),
        ];
        app.set_catalog(
            Rc::new(VecModel::from(vec![MediaCard {
                id: "delivery".into(),
                poster_path: url.into(),
                ..Default::default()
            }]))
            .into(),
        );
        assert_eq!(
            bridge.queue_poster_intake(Surface::Catalog, 0, true, String::new()),
            Admission::Queued
        );
        let mut state = bridge.poster_intake.0.lock().unwrap();
        assert!(state.active.is_empty());
        let request = state.high.pop_front().unwrap();
        state.refresh = false;
        state.deferred = false;
        state.active.insert(
            request.key.clone(),
            Active {
                ticket,
                live: Arc::new(AtomicBool::new(true)),
                handle: None,
            },
        );
        let delivery = Delivery {
            intake: bridge.poster_intake.clone(),
            key: request.key.clone(),
            ticket,
        };
        (request, delivery)
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn late_delivery_after_invalidation_or_close_never_publishes_or_recovers() {
        crate::app::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::late_delivery_after_invalidation_or_close_never_publishes_or_recovers"
            ),
            |fixture| {
                fixture.stop_actors();
                let bridge = &fixture.bridge;
                let app = bridge.app().unwrap();
                app.set_show_home(true); // Keep future Catalog discovery hidden.
                for mode in 0..3 {
                    let (request, delivery) = prepare_catalog_delivery(bridge, mode + 1);
                    assert!(bridge.intake_matches(&request) && delivery.is_live());
                    if mode == 2 {
                        bridge.poster_intake.close();
                    } else {
                        bridge.poster_intake.invalidate();
                    }
                    assert!(
                        bridge.intake_matches(&request),
                        "identity and authority stay unchanged: invalidation alone must reject this delivery"
                    );
                    assert!(!delivery.is_live());
                    let result = if mode == 1 {
                        Err(ImageFetchFailure::SourceFailure)
                    } else {
                        Ok(SharedPixelBuffer::new(2, 3))
                    };
                    bridge.complete_poster_delivery(request, delivery, 0, result);
                    assert!(!app.get_catalog().row_data(0).unwrap().is_loaded);
                    let state = bridge.poster_intake.0.lock().unwrap();
                    assert!(
                        state.active.is_empty()
                            && state.high.is_empty()
                            && state.low.is_empty()
                            && state.failures.is_empty()
                    );
                    drop(state);
                    let state = bridge.shared.lock().unwrap();
                    assert!(
                        state.artwork_recovery.is_empty()
                            && state.pending_artwork_recovery.is_empty()
                    );
                }
            },
        );
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn stale_catalog_authority_discards_pixels_and_recaptures_the_current_viewport() {
        crate::app::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::stale_catalog_authority_discards_pixels_and_recaptures_the_current_viewport"
            ),
            |fixture| {
                fixture.stop_actors();
                let bridge = &fixture.bridge;
                let app = bridge.app().unwrap();
                app.set_show_home(false);
                app.set_more_open(false);
                app.set_profiles_require_selection(false);
                set_active_cache_settings(CacheSettings {
                    cache_images: false,
                    lru_cache_mb: 32,
                    ..Default::default()
                });
                let (request, delivery) = prepare_catalog_delivery(bridge, 1);
                let old_authority = request.key.authority.clone();
                let context = profiles::capture();
                context
                    .session
                    .as_ref()
                    .unwrap()
                    .providers
                    .get()
                    .unwrap()
                    .with(|| {
                        nova_providers::plugin_settings::apply_settings_projection(|| {
                            Ok(r#"{"fixture":{"changed":true}}"#.into())
                        })
                        .unwrap();
                    });
                assert!(
                    bridge.intake_slot_matches(&request),
                    "neither catalog identity, profile nor image recipe changed"
                );
                assert!(!request.owner.is_current());
                decoded_cache_insert(
                    &image_cache_key(&request.key.url, ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE))),
                    SharedPixelBuffer::new(4, 5),
                );
                bridge.complete_poster_delivery(
                    request,
                    delivery,
                    0,
                    Ok(SharedPixelBuffer::new(2, 3)),
                );
                assert!(
                    !app.get_catalog().row_data(0).unwrap().is_loaded,
                    "old-authority pixels cannot paint"
                );
                {
                    let state = bridge.poster_intake.0.lock().unwrap();
                    assert_eq!(
                        state.active.len(),
                        1,
                        "current viewport must receive a fresh request without a scroll or external invalidation"
                    );
                    assert_ne!(state.active.keys().next().unwrap().authority, old_authority);
                    assert!(state.failures.is_empty());
                }
                // Close before queued UI delivery, then drain the real cached job.
                bridge.poster_intake.close();
                let until = Instant::now() + Duration::from_secs(3);
                while !bridge.poster_intake.0.lock().unwrap().active.is_empty() {
                    assert!(
                        Instant::now() < until,
                        "cancelled fresh request must complete"
                    );
                    profiles::invoke_account(|| {
                        slint::quit_event_loop().unwrap();
                    })
                    .unwrap();
                    slint::run_event_loop().unwrap();
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(!app.get_catalog().row_data(0).unwrap().is_loaded);
            },
        );
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn image_authority_batches_are_lazy_and_cannot_cross_recipe_or_profile_changes() {
        crate::app::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::image_authority_batches_are_lazy_and_cannot_cross_recipe_or_profile_changes"
            ),
            |fixture| {
                fixture.stop_actors();
                let bridge = &fixture.bridge;
                let app = bridge.app().unwrap();
                app.set_show_home(false);
                app.set_profiles_require_selection(false);
                bridge.shared.lock().unwrap().previews = (0..2)
                    .map(|index| {
                        serde_json::from_value::<MetaPreview>(serde_json::json!({
                            "id":format!("batch-{index}"), "type":"movie", "name":"Batch",
                            "poster":format!("https://fixture.invalid/batch-{index}.png")
                        }))
                        .unwrap()
                    })
                    .collect();
                app.set_catalog(
                    Rc::new(VecModel::from(
                        (0..2)
                            .map(|index| MediaCard {
                                id: format!("batch-{index}").into(),
                                poster_path: format!("https://fixture.invalid/batch-{index}.png")
                                    .into(),
                                ..Default::default()
                            })
                            .collect::<Vec<_>>(),
                    ))
                    .into(),
                );
                OWNER_CAPTURES.store(0, Ordering::Relaxed);
                let batch = Batch::default();
                assert_eq!(
                    bridge.queue_poster_batch(&batch, Surface::Catalog, 99, true, String::new()),
                    Admission::Obsolete
                );
                assert_eq!(
                    OWNER_CAPTURES.load(Ordering::Relaxed),
                    0,
                    "invalid rows cannot read/hash addon inventory"
                );
                for index in 0..2 {
                    assert_eq!(
                        bridge.queue_poster_batch(
                            &batch,
                            Surface::Catalog,
                            index,
                            true,
                            String::new()
                        ),
                        Admission::Queued
                    );
                }
                assert_eq!(
                    OWNER_CAPTURES.load(Ordering::Relaxed),
                    1,
                    "multiple admitted rows share one captured inventory"
                );
                profiles::with_context(profiles::context_for("foreign-owner"), || {
                    assert_eq!(
                        bridge.queue_poster_batch(&batch, Surface::Catalog, 0, true, String::new()),
                        Admission::Obsolete
                    );
                });
                let mut settings = active_cache_settings();
                settings.enabled = !settings.enabled;
                set_active_cache_settings(settings);
                assert_eq!(
                    bridge.queue_poster_batch(&batch, Surface::Catalog, 0, true, String::new()),
                    Admission::Obsolete,
                    "expired batch authority must not be silently recaptured"
                );
                assert_eq!(OWNER_CAPTURES.load(Ordering::Relaxed), 1);
                let fresh = Batch::default();
                assert_eq!(
                    bridge.queue_poster_batch(&fresh, Surface::Catalog, 0, true, String::new()),
                    Admission::Queued
                );
                assert_eq!(
                    OWNER_CAPTURES.load(Ordering::Relaxed),
                    2,
                    "a later dispatch captures fresh recipe authority"
                );
                bridge.poster_intake.close();
                assert_eq!(
                    bridge.queue_poster_batch(
                        &Batch::default(),
                        Surface::Catalog,
                        0,
                        true,
                        String::new()
                    ),
                    Admission::Closed
                );
                assert_eq!(
                    OWNER_CAPTURES.load(Ordering::Relaxed),
                    2,
                    "shutdown rejects before source inventory work"
                );
            },
        );
    }

    #[test]
    fn navigation_focus_frees_the_foreground_lane_without_releasing_physical_work() {
        let intake = Arc::new(Intake::default());
        let active = request(999, true).key;
        let live = Arc::new(AtomicBool::new(true));
        intake.0.lock().unwrap().active.insert(
            active.clone(),
            Active {
                ticket: 1,
                live: live.clone(),
                handle: None,
            },
        );
        let delivery = Delivery {
            intake: intake.clone(),
            key: active,
            ticket: 1,
        };
        for index in 0..VISIBLE {
            assert_eq!(intake.submit(request(index, true)), Admission::Queued);
        }
        let mut catalog = request(1000, false);
        catalog.key.surface = Surface::Catalog;
        assert_eq!(intake.submit(catalog.clone()), Admission::Queued);
        intake.focus(Foreground::Catalog);
        assert!(delivery.is_live() && live.load(Ordering::Acquire));
        {
            let state = intake.0.lock().unwrap();
            assert!(state.high.is_empty());
            assert_eq!(state.low.len(), 1, "background work remains retained");
            assert_eq!(state.active.len(), 1, "physical lease remains admitted");
        }
        catalog.visible = true;
        assert_eq!(
            intake.submit(catalog),
            Admission::Promoted,
            "new viewport can enter without waiting behind 128 hidden rows"
        );
        let mut background = request(1001, false);
        background.key.surface = Surface::Catalog;
        intake.submit(background);
        intake.focus(Foreground::Home);
        assert!(delivery.is_live());
        assert_eq!(intake.submit(request(2000, true)), Admission::Queued);
        let state = intake.0.lock().unwrap();
        assert_eq!(state.high.front().unwrap().key.surface, Surface::Continue);
        assert_eq!(state.low.len(), 1);
        assert_eq!(state.active.len(), 1);
    }

    #[test]
    fn full_visible_lane_keeps_background_promotion_retryable() {
        let intake = Intake::default();
        intake.submit(request(999, false));
        for index in 0..VISIBLE {
            assert_eq!(intake.submit(request(index, true)), Admission::Queued);
        }
        assert_eq!(
            intake.submit(request(999, true)),
            Admission::DeferredCapacity
        );
        assert_eq!(intake.0.lock().unwrap().low.len(), 1);
        intake.0.lock().unwrap().high.pop_front();
        assert_eq!(intake.submit(request(999, true)), Admission::Promoted);
        let state = intake.0.lock().unwrap();
        assert!(state.low.is_empty() && state.deferred);
        assert_eq!(state.high.len(), VISIBLE);
    }

    #[test]
    fn intake_coalescing_keeps_profile_source_recipe_and_authority_distinct() {
        let intake = Intake::default();
        intake.submit(request(1, true));
        for variant in 0..8 {
            let mut request = request(1, true);
            match variant {
                0 => request.key.profile = "bob".into(),
                1 => request.key.epoch += 1,
                2 => request.key.generation += 1,
                3 => request.key.media_type = "movie".into(),
                4 => request.key.source = "another source".into(),
                5 => request.key.projection = "another projection".into(),
                6 => request.key.recipe = "another recipe".into(),
                _ => request.key.authority.0 += 1,
            }
            assert_eq!(intake.submit(request), Admission::Queued);
        }
        assert_eq!(intake.0.lock().unwrap().high.len(), 9);
    }

    fn request(index: usize, visible: bool) -> Request {
        Request {
            key: Key {
                profile: "alice".into(),
                epoch: 1,
                surface: Surface::Continue,
                generation: 1,
                index,
                media_type: "series".into(),
                id: index.to_string(),
                source: "library".into(),
                projection: "episode-1".into(),
                url: format!("https://fixture.invalid/{index}"),
                fallback: String::new(),
                recipe: "poster".into(),
                authority: (1, 1, "inventory".into()),
            },
            context: profiles::capture(),
            owner: ImageFetchOwner::capture(),
            visible,
            fallback_attempt: false,
        }
    }
    #[test]
    fn async_intake_bounds_exact_dedup_and_visible_promotion() {
        let intake = Intake::default();
        assert_eq!(intake.submit(request(1, false)), Admission::Queued);
        assert_eq!(intake.submit(request(1, false)), Admission::Queued);
        assert_eq!(intake.submit(request(1, true)), Admission::Promoted);
        assert_eq!(intake.0.lock().unwrap().low.len(), 0);
        for index in 2..=VISIBLE {
            assert_eq!(intake.submit(request(index, true)), Admission::Queued);
        }
        assert_eq!(
            intake.submit(request(999, true)),
            Admission::DeferredCapacity
        );
        for index in 1000..1000 + BACKGROUND {
            assert_eq!(intake.submit(request(index, false)), Admission::Queued);
        }
        assert_eq!(
            intake.submit(request(2000, false)),
            Admission::DeferredCapacity
        );
        let state = intake.0.lock().unwrap();
        assert_eq!(state.high.len() + state.low.len(), 384);
        assert!(state.deferred);
    }
    #[test]
    fn async_intake_busy_waits_for_revision_without_losing_external_release() {
        let intake = Intake::default();
        assert!(!intake.needs_pump(|| panic!("settled intake must not read leaf availability")));
        intake.submit(request(1, true));
        // Record BEFORE a Busy attempt. External work may release its slot
        // before the synchronous Busy callback is acknowledged by the UI.
        intake.0.lock().unwrap().busy_revision = Some(7);
        assert!(!intake.needs_pump(|| 7));
        assert!(!intake.begin_pump(|| 7));
        assert!(intake.needs_pump(|| 8));
        assert!(intake.begin_pump(|| 8));
        assert!(intake.0.lock().unwrap().busy_revision.is_none());
        assert!(
            intake.0.lock().unwrap().failures.is_empty(),
            "Busy is not a source failure"
        );
    }
    #[test]
    fn async_intake_delivery_guard_releases_on_panic_and_close_cancels_active() {
        let intake = Arc::new(Intake::default());
        let key = request(1, true).key;
        let live = Arc::new(AtomicBool::new(true));
        intake.0.lock().unwrap().active.insert(
            key.clone(),
            Active {
                ticket: 1,
                live: live.clone(),
                handle: None,
            },
        );
        let guard = Delivery {
            intake: intake.clone(),
            key: key.clone(),
            ticket: 1,
        };
        intake.submit(request(2, true));
        intake.close();
        assert!(!live.load(Ordering::Acquire));
        assert!(intake.0.lock().unwrap().high.is_empty());
        assert_eq!(
            intake.0.lock().unwrap().active.len(),
            1,
            "cancelled physical work keeps its lease"
        );
        assert_eq!(intake.submit(request(3, true)), Admission::Closed);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = guard;
            panic!("injected delivery failure");
        }));
        assert!(result.is_err());
        assert!(intake.0.lock().unwrap().active.is_empty());
    }
    #[test]
    fn async_intake_old_ticket_cannot_release_replacement_or_typed_request() {
        let intake = Arc::new(Intake::default());
        let key = request(1, true).key;
        intake.0.lock().unwrap().active.insert(
            key.clone(),
            Active {
                ticket: 2,
                live: Arc::new(AtomicBool::new(true)),
                handle: None,
            },
        );
        drop(Delivery {
            intake: intake.clone(),
            key: key.clone(),
            ticket: 1,
        });
        assert_eq!(intake.0.lock().unwrap().active.len(), 1);
        assert_eq!(intake.submit(request(1, true)), Admission::AlreadyActive);
        let mut typed = request(1, true);
        typed.key.media_type = "movie".into();
        assert_eq!(intake.submit(typed), Admission::Queued);
        assert_eq!(intake.0.lock().unwrap().high.len(), 1);
    }
}

//! Async application admission in front of MED-07's existing artwork workers.
//! These are delivery leases, not additional fetch/decode threads.
use super::*;
use nova_media::net::{ImageFetchFailure, ImageFetchOwner};
use std::time::Instant;

const VISIBLE: usize = 128;
const BACKGROUND: usize = 256;
const DELIVERIES: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Surface {
    Catalog,
    Continue,
    New,
    Upcoming,
    HomeCatalog,
    History,
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
    fn intake_matches(&self, request: &Request) -> bool {
        profiles::is_current(&request.context)
            && !request
                .context
                .session
                .as_ref()
                .is_some_and(|session| session.cancelled.load(Ordering::Acquire))
            && self.intake_key_matches(&request.key)
    }
    // Desktop catalogs use the dedicated poster queue; Home batches admission.
    #[cfg(not(feature = "desktop"))]
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

    /// Home admits a bounded batch before pumping, rather than validating the
    /// entire pending queue once for every row in its initial model.
    pub(super) fn queue_poster_intake(
        &self,
        surface: Surface,
        index: usize,
        visible: bool,
        fallback: String,
    ) -> Admission {
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
        let (settings, inventory) = if profiles::enabled() {
            let Some(session) = context.session.as_ref() else {
                return Admission::Obsolete;
            };
            let Some(owner) = session.providers.get() else {
                return Admission::Obsolete;
            };
            let settings = owner.with(nova_providers::plugin_settings::capture_settings_capability);
            let Ok(inventory) = session.provider_inventory() else {
                return Admission::Obsolete;
            };
            (settings, inventory)
        } else {
            (
                nova_providers::plugin_settings::capture_settings_capability(),
                nova_providers::OwnedProviderSnapshot::capture(),
            )
        };
        let owner = ImageFetchOwner::new(settings, inventory);
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
            #[cfg(not(feature = "desktop"))]
            if !app.get_show_home()
                && !app.get_show_library()
                && !app.get_more_open()
                && !app.get_show_settings()
                && !app.get_show_about()
                && !app.get_show_calendar()
                && !app.get_show_downloads()
                && !app.get_player_open()
                && !app.get_modal_visible()
                && !app.get_profiles_require_selection()
                && !app.get_discover_search_open()
            {
                let model = app.get_catalog();
                let range = self
                    .shared
                    .lock()
                    .unwrap()
                    .catalog_page
                    .poster_retry_range(model.row_count());
                for index in range {
                    if model
                        .row_data(index)
                        .is_some_and(|row| !row.is_loaded && !row.poster_path.is_empty())
                    {
                        if self.enqueue_poster_intake(Surface::Catalog, index, true, String::new())
                            == Admission::DeferredCapacity
                        {
                            break;
                        }
                    }
                }
            }
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
                    // invoke_account always runs cleanup; publication is checked
                    // against the captured owner and current exact model separately.
                    let _ = profiles::invoke_account(move || {
                        let current = bridge.intake_matches(&request);
                        drop(delivery);
                        if current {
                            match result {
                                Err(ImageFetchFailure::Busy) => {
                                    bridge.poster_intake.submit(request);
                                    bridge.poster_intake.0.lock().unwrap().busy_revision =
                                        Some(before_attempt);
                                    return;
                                }
                                Err(ImageFetchFailure::SourceFailure)
                                    if !request.fallback_attempt
                                        && !request.key.fallback.is_empty()
                                        && request.key.fallback != request.key.url =>
                                {
                                    let mut request = request;
                                    request.fallback_attempt = true;
                                    bridge.poster_intake.submit(request);
                                }
                                Ok(pixels) => bridge.publish_intake_poster(&request.key, pixels),
                                Err(ImageFetchFailure::Cancelled) => {}
                                Err(_) => {
                                    {
                                        let mut state = bridge.poster_intake.0.lock().unwrap();
                                        if state.failures.len() == VISIBLE + BACKGROUND {
                                            state.failures.pop_front();
                                        }
                                        state
                                            .failures
                                            .push_back((request.key.clone(), Instant::now()));
                                    }
                                    profiles::with_context(request.context.clone(), || {
                                        if request.key.surface == Surface::Catalog {
                                            bridge.recover_catalog_artwork(
                                                request.key.generation,
                                                request.key.index,
                                                &request.key.url,
                                            );
                                        } else {
                                            bridge.recover_missing_artwork(
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
                        profiles::with_context(profiles::active_context(), || {
                            bridge.pump_poster_intake()
                        });
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
            PosterTx::closed(),
            Arc::new(AtomicU64::new(0)),
            crate::player::Player::setup(&app),
            DownloadCoordinator::new(root.join("downloads")),
        );
        let owner = RunOwner(bridge.poster_intake.clone());

        // Occupy both REAL media artwork workers, not the app delivery leases.
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
            for _ in 0..2 {
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
        for index in 0..2 {
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
        for _ in 0..2 {
            observed.recv_timeout(Duration::from_secs(8)).unwrap();
        }
        // All32 pending slots belong to other consumers. Cancelled jobs still
        // occupy a queued slot until a worker pops and terminally completes them.
        for index in 0..32 {
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
            "both workers remain behind the HTTP barrier"
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
                    && finished_tick.load(Ordering::SeqCst) == 34
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
            34,
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
            PosterTx::closed(),
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

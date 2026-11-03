//! Calendar/Downloads own only retained UI images. This shared scheduler keeps
//! request metadata transient and bounds page image transport to four workers.
use super::*;
use std::time::Instant;

type Pixels = SharedPixelBuffer<Rgba8Pixel>;
const WORKERS: usize = 4;
const RETRY: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Page {
    Calendar,
    Downloads,
}

/// The resolved cache key includes the complete recipe, not just its size.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Identity {
    pub url: String,
    pub cache_key: String,
    image_use: ImageUse,
}
impl Identity {
    pub fn new(url: String, image_use: ImageUse) -> Self {
        let cache_key = image_cache_key(&url, image_use);
        Self {
            url,
            cache_key,
            image_use,
        }
    }
    fn current(&self) -> bool {
        self.cache_key == image_cache_key(&self.url, self.image_use)
    }
}

#[derive(Default)]
struct Row {
    identity: Option<Identity>,
    retained: bool,
    displayed: Option<Identity>,
    loading: Option<Arc<AtomicBool>>,
}
impl Row {
    fn cancel(&mut self) {
        if let Some(live) = self.loading.take() {
            live.store(false, Ordering::Release);
        }
    }
}
#[derive(Default)]
struct View {
    generation: u64,
    context: String,
    active: bool,
    rows: HashMap<String, Row>,
}
#[derive(Default)]
pub(super) struct PageArtworkState {
    views: HashMap<Page, View>,
}
impl PageArtworkState {
    /// Rebuilds invalidate pending deliveries only when the view or its
    /// identities change; a Downloads progress tick keeps completed art alive.
    pub fn reconcile(
        &mut self,
        page: Page,
        context: String,
        rows: Vec<(String, Identity)>,
        active: bool,
    ) {
        let view = self.views.entry(page).or_default();
        let changed = view.context != context
            || view.rows.len() != rows.len()
            || rows.iter().any(|(key, identity)| {
                view.rows
                    .get(key)
                    .is_none_or(|row| row.identity.as_ref() != Some(identity))
            });
        if changed {
            view.generation = view.generation.wrapping_add(1);
            view.context = context;
            for row in view.rows.values_mut() {
                row.cancel();
            }
        }
        let keys: HashSet<_> = rows.iter().map(|(key, _)| key.clone()).collect();
        view.rows.retain(|key, _| keys.contains(key));
        for (key, identity) in rows {
            let row = view.rows.entry(key).or_default();
            if row.identity.as_ref() != Some(&identity) {
                row.cancel();
                row.displayed = None;
            }
            row.identity = Some(identity);
        }
        view.active = active;
    }
    fn retain(&mut self, page: Page, key: &str, retained: bool) {
        if let Some(view) = self.views.get_mut(&page)
            && let Some(row) = view.rows.get_mut(key)
        {
            row.retained = retained && view.active;
            if !row.retained {
                row.cancel();
                row.displayed = None;
            }
        }
    }
    fn activate(&mut self, page: Page) {
        if let Some(view) = self.views.get_mut(&page)
            && !view.active
        {
            view.active = true;
            view.generation = view.generation.wrapping_add(1);
        }
    }
    fn deactivate(&mut self, page: Page) {
        if let Some(view) = self.views.get_mut(&page) {
            view.active = false;
            view.generation = view.generation.wrapping_add(1);
            for row in view.rows.values_mut() {
                row.cancel();
                row.retained = false;
                row.displayed = None;
            }
        }
    }
    pub fn retained_keys(&self, page: Page) -> HashSet<String> {
        self.views
            .get(&page)
            .filter(|view| view.active)
            .map(|view| {
                view.rows
                    .iter()
                    .filter(|(_, row)| row.retained)
                    .map(|(key, _)| key.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
    fn request(&mut self, page: Page, key: &str) -> Option<Request> {
        let view = self.views.get_mut(&page)?;
        let row = view.rows.get_mut(key)?;
        let identity = row.identity.clone()?;
        if !view.active
            || !row.retained
            || row.loading.is_some()
            || row.displayed.as_ref() == Some(&identity)
            || identity.url.is_empty()
            || !identity.current()
        {
            return None;
        }
        let live = Arc::new(AtomicBool::new(true));
        row.loading = Some(live.clone());
        Some(Request {
            page,
            key: key.into(),
            generation: view.generation,
            identity,
            settings_revision: image_settings_revision(),
            live,
        })
    }
    fn finish(&mut self, request: &Request, loaded: bool) -> bool {
        let Some(view) = self.views.get_mut(&request.page) else {
            return false;
        };
        let Some(row) = view.rows.get_mut(&request.key) else {
            return false;
        };
        // Even a rejected recipe revision releases its own loading flag so
        // the next retained-card tick can request the current pixels again.
        if row
            .loading
            .as_ref()
            .is_some_and(|live| Arc::ptr_eq(live, &request.live))
        {
            row.loading = None;
        }
        if !view.active
            || image_settings_revision() != request.settings_revision
            || view.generation != request.generation
            || !row.retained
            || row.identity.as_ref() != Some(&request.identity)
            || !request.identity.current()
            || !request.live.load(Ordering::Acquire)
        {
            return false;
        }
        row.loading = None;
        if loaded {
            row.displayed = Some(request.identity.clone());
        }
        true
    }
}

struct Request {
    page: Page,
    key: String,
    generation: u64,
    identity: Identity,
    settings_revision: u64,
    live: Arc<AtomicBool>,
}
struct Subscriber {
    request: Request,
    context: profiles::Context,
    owner: Result<nova_media::net::ImageFetchOwner, String>,
    deliver: Box<dyn FnOnce(Request, Delivery) + Send>,
}
impl Subscriber {
    fn dispatch(self, delivery: Delivery) {
        // One transport may serve rows belonging to different profiles. Its
        // launching worker does not own every subscriber's UI continuation.
        profiles::with_context(self.context, || (self.deliver)(self.request, delivery));
    }
}
/// A transport slot remains reserved until every subscriber has delivered or
/// discarded its shared completion. Failed dispatch and early UI returns drop
/// the same acknowledgement, so neither path can strand a lane.
struct CompletionAck {
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}
impl CompletionAck {
    fn new(release: impl FnOnce() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            release: Some(Box::new(release)),
        })
    }
}
impl Drop for CompletionAck {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}
struct Delivery {
    pixels: Option<Pixels>,
    _acknowledgement: Arc<CompletionAck>,
}
impl Drop for Delivery {
    fn drop(&mut self) {
        // Also force a whole-envelope closure capture: a failed invoke must
        // discard the pixels before releasing its slot, not detach the guard.
        drop(self.pixels.take());
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct QueueIdentity {
    image: Identity,
    profile: String,
    epoch: u64,
    authority: Option<(u64, u64, Arc<str>)>,
}
impl std::ops::Deref for QueueIdentity {
    type Target = Identity;
    fn deref(&self) -> &Identity {
        &self.image
    }
}

fn capture_image_owner(
    context: &profiles::Context,
) -> Result<nova_media::net::ImageFetchOwner, String> {
    if let Some(session) = &context.session {
        let settings = session
            .providers
            .get()
            .map(|owner| owner.with(nova_providers::plugin_settings::capture_settings_capability))
            .ok_or("profile providers unavailable")?;
        return Ok(nova_media::net::ImageFetchOwner::new(
            settings,
            session.provider_inventory()?,
        ));
    }
    if profiles::enabled() {
        return Err("profile session unavailable".into());
    }
    Ok(nova_media::net::ImageFetchOwner::capture())
}

struct Work {
    identity: QueueIdentity,
    context: profiles::Context,
    owner: Result<nova_media::net::ImageFetchOwner, String>,
    subscribers: Vec<Subscriber>,
}
#[derive(Default)]
struct Queue {
    work: HashMap<QueueIdentity, Work>,
    pending: VecDeque<QueueIdentity>,
    failed: HashMap<QueueIdentity, Instant>,
    active: usize,
}
static QUEUE: LazyLock<Mutex<Queue>> = LazyLock::new(|| Mutex::new(Queue::default()));

impl Queue {
    fn add(&mut self, subscriber: Subscriber) -> bool {
        let identity = QueueIdentity {
            image: subscriber.request.identity.clone(),
            profile: subscriber.context.id.clone(),
            epoch: subscriber.context.epoch,
            authority: subscriber
                .owner
                .as_ref()
                .ok()
                .map(|owner| owner.generation()),
        };
        let now = Instant::now();
        self.failed
            .retain(|_, time| now.duration_since(*time) < RETRY);
        if self.failed.contains_key(&identity) {
            return false;
        }
        if let Some(work) = self.work.get_mut(&identity) {
            work.subscribers.push(subscriber);
        } else {
            self.pending.push_back(identity.clone());
            self.work.insert(
                identity.clone(),
                Work {
                    identity,
                    context: subscriber.context.clone(),
                    owner: subscriber.owner.clone(),
                    subscribers: vec![subscriber],
                },
            );
        }
        true
    }
    fn next(&mut self) -> Option<QueueIdentity> {
        if self.active >= WORKERS {
            return None;
        }
        while let Some(identity) = self.pending.pop_front() {
            let Some(work) = self.work.get_mut(&identity) else {
                continue;
            };
            work.subscribers
                .retain(|subscriber| subscriber.request.live.load(Ordering::Acquire));
            if work.subscribers.is_empty() {
                self.work.remove(&identity);
                continue;
            }
            self.active += 1;
            return Some(work.identity.clone());
        }
        None
    }
}
// A completion can reject synchronously when the leaf executor is full or
// unavailable. Suppress same-stack pumping only: the existing outer loop drains
// released lanes, while other worker threads retain their independent pumps.
pub(super) fn with_pump_guard(
    local: &'static std::thread::LocalKey<std::cell::Cell<bool>>,
    run: impl FnOnce(),
) {
    local.with(|active| {
        if active.replace(true) {
            return;
        }
        struct Reset<'a>(&'a std::cell::Cell<bool>);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let _reset = Reset(active);
        run();
    });
}

fn pump() {
    thread_local! { static PUMPING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
    with_pump_guard(&PUMPING, || {
        loop {
            let next = {
                let mut queue = QUEUE.lock().unwrap();
                queue.next().map(|key| {
                    let work = &queue.work[&key];
                    (key, work.owner.clone(), work.context.clone())
                })
            };
            let Some((identity, owner, context)) = next else {
                return;
            };
            let worker_revision = image_settings_revision();
            // The scheduler owns process-wide lanes. Always receive transport
            // completion, including when the launching profile has been replaced;
            // subscriber UI guards discard stale pixels while their RAII ack frees
            // the lane after the last delivery is consumed or dropped.
            let live_key = identity.clone();
            let url = identity.url.clone();
            let image_use = identity.image_use;
            let complete = move |pixels: Option<Pixels>| {
                let subscribers = {
                    let mut queue = QUEUE.lock().unwrap();
                    if pixels.is_none() {
                        queue.failed.insert(identity.clone(), Instant::now());
                    }
                    queue
                        .work
                        .remove(&identity)
                        .map(|work| work.subscribers)
                        .unwrap_or_default()
                };
                let acknowledgement = CompletionAck::new(|| {
                    {
                        let mut queue = QUEUE.lock().unwrap();
                        queue.active -= 1;
                    }
                    pump();
                });
                for subscriber in subscribers {
                    if !subscriber.request.live.load(Ordering::Acquire) {
                        continue;
                    }
                    let delivered = if subscriber.request.settings_revision == worker_revision {
                        pixels.clone()
                    } else {
                        None
                    };
                    subscriber.dispatch(Delivery {
                        pixels: delivered,
                        _acknowledgement: acknowledgement.clone(),
                    });
                }
                // Clones share one backing buffer. Drop the transport's owner
                // before the final acknowledgement can admit another worker.
                drop(pixels);
                drop(acknowledgement);
            };
            match owner {
                Ok(owner) => {
                    nova_media::net::fetch_image_for_owner_controlled(
                        url,
                        image_use,
                        owner,
                        Arc::new(move || {
                            profiles::is_current(&context)
                                && !context.session.as_ref().is_some_and(|session| {
                                    session.cancelled.load(Ordering::Acquire)
                                })
                                && QUEUE
                                    .lock()
                                    .unwrap()
                                    .work
                                    .get(&live_key)
                                    .is_some_and(|work| {
                                        work.subscribers.iter().any(|subscriber| {
                                            subscriber.request.live.load(Ordering::Acquire)
                                        })
                                    })
                        }),
                        complete,
                    );
                }
                Err(_) => complete(None),
            }
        }
    });
}
fn paint(app: &AppWindow, page: Page, key: &str, pixels: Option<Pixels>, identity: &str) {
    match page {
        Page::Calendar => {
            let Some((list, release_key)) = key.split_at_checked(1) else {
                return;
            };
            let model = if list == "l" {
                app.get_calendar_later()
            } else {
                app.get_calendar_rows()
            };
            for index in 0..model.row_count() {
                if let Some(mut row) = model.row_data(index)
                    && row.key == release_key
                    && (identity.is_empty() || row.artwork_identity == identity)
                {
                    row.is_loaded = pixels.is_some();
                    row.poster = pixels.clone().map(Image::from_rgba8).unwrap_or_default();
                    model.set_row_data(index, row);
                }
            }
        }
        Page::Downloads => {
            let model = app.get_downloads_page_groups();
            for index in 0..model.row_count() {
                if let Some(mut row) = model.row_data(index)
                    && row.id == key
                    && (identity.is_empty() || row.artwork_identity == identity)
                {
                    row.artwork = pixels.clone().map(Image::from_rgba8).unwrap_or_default();
                    model.set_row_data(index, row);
                }
            }
        }
    }
}
/// Invalidate subscriptions before calling this: clearing retained pixels must
/// never allow an older in-flight delivery to repaint a hidden page.
fn clear_page_artwork(app: &AppWindow, page: Page) {
    match page {
        Page::Calendar => {
            for model in [app.get_calendar_rows(), app.get_calendar_later()] {
                for index in 0..model.row_count() {
                    if let Some(mut row) = model.row_data(index)
                        && (row.is_loaded || row.poster.size().width != 0)
                    {
                        row.is_loaded = false;
                        row.poster = Image::default();
                        model.set_row_data(index, row);
                    }
                }
            }
        }
        Page::Downloads => {
            let model = app.get_downloads_page_groups();
            for index in 0..model.row_count() {
                if let Some(mut row) = model.row_data(index)
                    && row.artwork.size().width != 0
                {
                    row.artwork = Image::default();
                    model.set_row_data(index, row);
                }
            }
        }
    }
}

impl Bridge {
    pub(super) fn calendar_artwork_visible(&self, key: &str, retained: bool) {
        self.page_artwork_visible(Page::Calendar, key, retained);
    }
    pub(super) fn downloads_artwork_visible(&self, key: &str, retained: bool) {
        self.page_artwork_visible(Page::Downloads, key, retained);
    }
    pub(super) fn calendar_artwork_active(&self, active: bool) {
        self.page_artwork_active(Page::Calendar, active);
    }
    pub(super) fn downloads_artwork_active(&self, active: bool) {
        self.page_artwork_active(Page::Downloads, active);
    }
    fn page_artwork_active(&self, page: Page, active: bool) {
        let Some(app) = self.app() else { return };
        let shown = match page {
            Page::Calendar => app.get_show_calendar(),
            Page::Downloads => app.get_show_downloads(),
        };
        if active && shown && app.get_page_artwork_allowed() {
            self.shared.lock().unwrap().page_artwork.activate(page);
            self.schedule_page_artwork(page);
            return;
        }
        self.shared.lock().unwrap().page_artwork.deactivate(page);
        clear_page_artwork(&app, page);
    }
    fn page_artwork_visible(&self, page: Page, key: &str, retained: bool) {
        let Some(app) = self.app() else { return };
        let active = app.get_page_artwork_allowed()
            && match page {
                Page::Calendar => app.get_show_calendar(),
                Page::Downloads => app.get_show_downloads(),
            };
        let retained = retained && active;
        let request = {
            let mut state = self.shared.lock().unwrap();
            state.page_artwork.retain(page, key, retained);
            state.page_artwork.request(page, key)
        };
        if !retained {
            paint(&app, page, key, None, "");
            return;
        }
        let Some(request) = request else { return };
        if let Some(pixels) = decoded_cache_get(&request.identity.cache_key) {
            if self
                .shared
                .lock()
                .unwrap()
                .page_artwork
                .finish(&request, true)
            {
                paint(&app, page, key, Some(pixels), &request.identity.cache_key);
            }
            return;
        }
        // A cooldown skips only this URL and never occupies a worker lane.
        let live = request.live.clone();
        let bridge = self.clone();
        let deliver = Box::new(move |request: Request, mut delivery: Delivery| {
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                let Some(app) = bridge.app() else { return };
                let active = app.get_page_artwork_allowed()
                    && match request.page {
                        Page::Calendar => app.get_show_calendar(),
                        Page::Downloads => app.get_show_downloads(),
                    };
                if !active
                    || !bridge
                        .shared
                        .lock()
                        .unwrap()
                        .page_artwork
                        .finish(&request, delivery.pixels.is_some())
                {
                    return;
                }
                // Completion pixels stay deliverable even with zero cache budget.
                if let Some(pixels) = delivery.pixels.take() {
                    paint(
                        &app,
                        request.page,
                        &request.key,
                        Some(pixels),
                        &request.identity.cache_key,
                    );
                }
            });
        });
        let context = profiles::capture();
        let owner = capture_image_owner(&context);
        if !QUEUE.lock().unwrap().add(Subscriber {
            request,
            context,
            owner,
            deliver,
        }) {
            live.store(false, Ordering::Release);
            if let Some(row) = self
                .shared
                .lock()
                .unwrap()
                .page_artwork
                .views
                .get_mut(&page)
                .and_then(|view| view.rows.get_mut(key))
            {
                row.loading = None;
            }
        }
        pump();
    }
    pub(super) fn schedule_page_artwork(&self, page: Page) {
        let keys: Vec<_> = self
            .shared
            .lock()
            .unwrap()
            .page_artwork
            .views
            .get(&page)
            .map(|view| {
                view.rows
                    .iter()
                    .filter(|(_, row)| row.retained)
                    .map(|(key, _)| key.clone())
                    .collect()
            })
            .unwrap_or_default();
        for key in keys {
            self.page_artwork_visible(page, &key, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use i_slint_core::model::{ModelChangeListener, ModelChangeListenerContainer};
    use std::cell::Cell;
    use std::pin::Pin;

    struct CountingModel<T> {
        rows: VecModel<T>,
        reads: Cell<usize>,
    }
    impl<T: Clone + 'static> CountingModel<T> {
        fn new(rows: Vec<T>) -> Self {
            Self {
                rows: VecModel::from(rows),
                reads: Cell::new(0),
            }
        }
    }
    impl<T: Clone + 'static> Model for CountingModel<T> {
        type Data = T;
        fn row_count(&self) -> usize {
            self.rows.row_count()
        }
        fn row_data(&self, index: usize) -> Option<T> {
            self.reads.set(self.reads.get() + 1);
            self.rows.row_data(index)
        }
        fn set_row_data(&self, index: usize, row: T) {
            self.rows.set_row_data(index, row);
        }
        fn model_tracker(&self) -> &dyn slint::ModelTracker {
            self.rows.model_tracker()
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[derive(Default)]
    struct Changes(Cell<usize>);
    impl ModelChangeListener for Changes {
        fn row_changed(self: Pin<&Self>, _: usize) {
            self.0.set(self.0.get() + 1);
        }
        fn row_added(self: Pin<&Self>, _: usize, _: usize) {
            panic!("clear inserted rows");
        }
        fn row_removed(self: Pin<&Self>, _: usize, _: usize) {
            panic!("clear removed rows");
        }
        fn reset(self: Pin<&Self>) {
            panic!("clear reset the model");
        }
    }
    fn observe_changes<T: Clone + 'static>(
        model: &CountingModel<T>,
    ) -> Pin<Box<ModelChangeListenerContainer<Changes>>> {
        let changes = Box::pin(ModelChangeListenerContainer::<Changes>::default());
        model
            .model_tracker()
            .attach_peer(changes.as_ref().model_peer());
        changes
    }

    #[test]
    fn clear_visits_each_row_once_notifies_only_changed_art_and_works_without_cache() {
        const NAME: &str = concat!(
            module_path!(),
            "::clear_visits_each_row_once_notifies_only_changed_art_and_works_without_cache"
        );
        const ISOLATED: &str = "NOVA_PAGE_ARTWORK_CLEAR_TEST";
        if std::env::var_os(ISOLATED).is_none() {
            // Slint backend and decoded-cache budget are process globals.
            // module_path includes the crate; libtest's filter does not.
            let test_name = NAME.split_once("::").unwrap().1;
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ISOLATED, "1")
                .args(["--exact", test_name, "--nocapture"])
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        i_slint_backend_testing::init_integration_test_with_mock_time();
        decoded_cache_clear();
        set_decoded_cache_budget(0);
        decoded_cache_insert("page-clear-zero-budget", Pixels::new(2, 2));
        assert_eq!(decoded_cache_stats(), (0, 0));
        let app = AppWindow::new().unwrap();
        for count in [100, 1_000, 10_000] {
            for percentage in [0, 10, 100] {
                let loaded = count * percentage / 100;
                let calendar_rows = || {
                    (0..count)
                        .map(|index| crate::CalendarRelease {
                            key: format!("row-{index}").into(),
                            artwork_identity: format!("identity-{index}").into(),
                            // Cover inconsistent flag/image states as well as the
                            // normal loaded state. Every affected row changes once.
                            is_loaded: index < loaded && index % 3 != 1,
                            poster: if index < loaded && index % 3 != 0 {
                                Image::from_rgba8(Pixels::new(2, 2))
                            } else {
                                Image::default()
                            },
                            ..Default::default()
                        })
                        .collect::<Vec<_>>()
                };
                let current = Rc::new(CountingModel::new(calendar_rows()));
                let later = Rc::new(CountingModel::new(calendar_rows()));
                let current_changes = observe_changes(current.as_ref());
                let later_changes = observe_changes(later.as_ref());
                app.set_calendar_rows(current.clone().into());
                app.set_calendar_later(later.clone().into());
                current.reads.set(0);
                later.reads.set(0);
                clear_page_artwork(&app, Page::Calendar);
                for (model, changes) in [(&current, &current_changes), (&later, &later_changes)] {
                    assert_eq!(model.reads.get(), count);
                    assert_eq!(changes.0.get(), loaded);
                    for (index, row) in model.rows.iter().enumerate() {
                        assert!(!row.is_loaded);
                        assert_eq!(row.key, format!("row-{index}"));
                        assert_eq!(row.poster.size().width, 0);
                        assert_eq!(row.artwork_identity, format!("identity-{index}"));
                    }
                    model.reads.set(0);
                }
                clear_page_artwork(&app, Page::Calendar);
                assert_eq!(current.reads.get(), count);
                assert_eq!(later.reads.get(), count);
                assert_eq!(current_changes.0.get(), loaded);
                assert_eq!(later_changes.0.get(), loaded);

                let downloads = Rc::new(CountingModel::new(
                    (0..count)
                        .map(|index| crate::DownloadGroup {
                            id: format!("row-{index}").into(),
                            artwork_identity: format!("identity-{index}").into(),
                            artwork: if index < loaded {
                                Image::from_rgba8(Pixels::new(2, 2))
                            } else {
                                Image::default()
                            },
                            ..Default::default()
                        })
                        .collect(),
                ));
                let changes = observe_changes(downloads.as_ref());
                app.set_downloads_page_groups(downloads.clone().into());
                downloads.reads.set(0);
                clear_page_artwork(&app, Page::Downloads);
                assert_eq!(downloads.reads.get(), count);
                assert_eq!(changes.0.get(), loaded);
                for (index, row) in downloads.rows.iter().enumerate() {
                    assert_eq!(row.id, format!("row-{index}"));
                    assert_eq!(row.artwork.size().width, 0);
                    assert_eq!(row.artwork_identity, format!("identity-{index}"));
                }
                downloads.reads.set(0);
                clear_page_artwork(&app, Page::Downloads);
                assert_eq!(downloads.reads.get(), count);
                assert_eq!(changes.0.get(), loaded);
                assert_eq!(decoded_cache_stats(), (0, 0));
            }
        }
    }

    #[test]
    fn both_pages_reject_deactivated_deliveries_and_request_current_art_after_return() {
        for page in [Page::Calendar, Page::Downloads] {
            let mut state = PageArtworkState::default();
            state.reconcile(page, "view".into(), rows(), true);
            state.retain(page, "r0", true);
            let stale = state.request(page, "r0").unwrap();
            state.deactivate(page);
            assert!(!stale.live.load(Ordering::Acquire));
            assert!(!state.finish(&stale, true));
            assert!(state.request(page, "r0").is_none());
            state.activate(page);
            state.retain(page, "r0", true);
            let current = state.request(page, "r0").unwrap();
            assert_ne!(current.generation, stale.generation);
            assert!(!state.finish(&stale, true));
            assert!(state.finish(&current, true));
        }
    }

    fn identity(url: &str) -> Identity {
        Identity::new(url.into(), ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)))
    }
    fn rows() -> Vec<(String, Identity)> {
        (0..150)
            .map(|index| (format!("r{index}"), identity(&format!("url-{index}"))))
            .collect()
    }
    #[test]
    fn scrolling_past_eighty_rows_requests_later_cards_and_releases_previous_pixels() {
        let mut state = PageArtworkState::default();
        state.reconcile(Page::Calendar, "agenda".into(), rows(), true);
        state.retain(Page::Calendar, "r0", true);
        let first = state.request(Page::Calendar, "r0").unwrap();
        assert!(state.finish(&first, true));
        assert!(state.request(Page::Calendar, "r0").is_none());
        state.retain(Page::Calendar, "r0", false);
        state.retain(Page::Calendar, "r120", true);
        assert!(state.request(Page::Calendar, "r120").is_some());
        assert_eq!(
            state.retained_keys(Page::Calendar),
            HashSet::from(["r120".into()])
        );
        let old = &state.views[&Page::Calendar].rows["r0"];
        assert!(old.displayed.is_none());
        assert!(old.loading.is_none());
        assert!(state.request(Page::Calendar, "r0").is_none());
    }
    #[test]
    fn stale_url_recipe_and_view_generation_cannot_publish() {
        let mut state = PageArtworkState::default();
        state.reconcile(
            Page::Calendar,
            "day-one".into(),
            vec![("r1".into(), identity("old"))],
            true,
        );
        state.retain(Page::Calendar, "r1", true);
        let old_url = state.request(Page::Calendar, "r1").unwrap();
        state.reconcile(
            Page::Calendar,
            "day-one".into(),
            vec![("r1".into(), identity("new"))],
            true,
        );
        assert!(!state.finish(&old_url, true));
        let old_view = state.request(Page::Calendar, "r1").unwrap();
        state.reconcile(
            Page::Calendar,
            "day-two".into(),
            vec![("r1".into(), identity("new"))],
            true,
        );
        assert!(!state.finish(&old_view, true));
        let old_recipe = state.request(Page::Calendar, "r1").unwrap();
        let recipe = Identity::new("new".into(), ImageUse::Fixed(Some(100)));
        state.reconcile(
            Page::Calendar,
            "day-two".into(),
            vec![("r1".into(), recipe)],
            true,
        );
        assert!(!state.finish(&old_recipe, true));
        let mut stale_revision = state.request(Page::Calendar, "r1").unwrap();
        stale_revision.settings_revision = stale_revision.settings_revision.wrapping_add(1);
        assert!(!state.finish(&stale_revision, true));
        let current = state.request(Page::Calendar, "r1").unwrap();
        assert!(state.finish(&current, true));
    }
    #[test]
    fn inactive_page_invalidates_pending_work_and_releases_display_identity() {
        let mut state = PageArtworkState::default();
        state.reconcile(Page::Downloads, "all".into(), rows(), true);
        state.retain(Page::Downloads, "r0", true);
        state.retain(Page::Downloads, "r1", true);
        let completed = state.request(Page::Downloads, "r0").unwrap();
        assert!(state.finish(&completed, true));
        let pending = state.request(Page::Downloads, "r1").unwrap();
        state.deactivate(Page::Downloads);
        assert!(!state.finish(&pending, true));
        assert!(state.retained_keys(Page::Downloads).is_empty());
        for row in state.views[&Page::Downloads].rows.values() {
            assert!(row.displayed.is_none());
            assert!(row.loading.is_none());
        }
    }
    #[test]
    fn overlay_deactivation_and_reactivation_reuses_rows_with_a_new_generation() {
        let mut state = PageArtworkState::default();
        let rows = vec![("group".into(), identity("logo"))];
        state.reconcile(Page::Downloads, "all".into(), rows.clone(), true);
        state.retain(Page::Downloads, "group", true);
        let stale = state.request(Page::Downloads, "group").unwrap();
        state.deactivate(Page::Downloads);
        assert!(state.request(Page::Downloads, "group").is_none());
        // A transfer tick underneath the modal preserves the existing model
        // but must not make hidden artwork active again.
        state.reconcile(Page::Downloads, "all".into(), rows, false);
        assert!(state.request(Page::Downloads, "group").is_none());
        state.activate(Page::Downloads);
        state.retain(Page::Downloads, "group", true);
        let current = state.request(Page::Downloads, "group").unwrap();
        assert_ne!(current.generation, stale.generation);
        assert!(!state.finish(&stale, true));
        assert!(state.finish(&current, true));
        assert_eq!(
            state.retained_keys(Page::Downloads),
            HashSet::from(["group".into()])
        );
    }

    fn queue_identity(url: &str) -> QueueIdentity {
        let subscriber = subscriber(url);
        QueueIdentity {
            image: subscriber.request.identity.clone(),
            profile: subscriber.context.id,
            epoch: subscriber.context.epoch,
            authority: subscriber
                .owner
                .as_ref()
                .ok()
                .map(|owner| owner.generation()),
        }
    }
    fn subscriber(url: &str) -> Subscriber {
        Subscriber {
            context: profiles::capture(),
            owner: Ok(nova_media::net::ImageFetchOwner::capture()),
            request: Request {
                page: Page::Calendar,
                key: url.into(),
                generation: 1,
                identity: identity(url),
                settings_revision: image_settings_revision(),
                live: Arc::new(AtomicBool::new(true)),
            },
            deliver: Box::new(|_, _| {}),
        }
    }
    #[test]
    fn completion_fanout_holds_a_slot_until_all_ui_deliveries_finish_or_drop() {
        let queue = Arc::new(Mutex::new(Queue::default()));
        {
            let mut queue = queue.lock().unwrap();
            for url in ["a", "b", "c", "d", "e"] {
                queue.add(subscriber(url));
            }
            for _ in 0..WORKERS {
                assert!(queue.next().is_some());
            }
            assert!(queue.next().is_none());
        }
        let released = Arc::new(AtomicUsize::new(0));
        let release_queue = queue.clone();
        let releases = released.clone();
        let acknowledgement = CompletionAck::new(move || {
            release_queue.lock().unwrap().active -= 1;
            releases.fetch_add(1, Ordering::SeqCst);
        });
        let pixels = SharedPixelBuffer::<Rgba8Pixel>::new(2, 2);
        let mut successful = Delivery {
            pixels: Some(pixels.clone()),
            _acknowledgement: acknowledgement.clone(),
        };
        let failed_dispatch = Delivery {
            pixels: Some(pixels.clone()),
            _acknowledgement: acknowledgement.clone(),
        };
        assert_eq!(
            successful.pixels.as_ref().unwrap().as_bytes().as_ptr(),
            failed_dispatch.pixels.as_ref().unwrap().as_bytes().as_ptr(),
            "fanout shares a single backing pixel buffer"
        );
        drop(pixels);
        drop(acknowledgement);
        assert!(
            queue.lock().unwrap().next().is_none(),
            "transport completion alone cannot admit worker five"
        );
        // invoke_from_event_loop failure (or cancellation before painting)
        // destroys the same envelope, without manually releasing twice.
        drop(failed_dispatch);
        assert_eq!(released.load(Ordering::SeqCst), 0);
        assert!(queue.lock().unwrap().next().is_none());
        drop(successful.pixels.take());
        drop(successful);
        assert_eq!(released.load(Ordering::SeqCst), 1);
        assert_eq!(queue.lock().unwrap().next().unwrap().url, "e");
    }

    #[test]
    fn partitioned_profiles_keep_each_context_and_release_independent_lanes() {
        let original_context = profiles::capture();
        let queue = Arc::new(Mutex::new(Queue::default()));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let retained = Arc::new(Mutex::new(None::<Delivery>));
        let mut outgoing =
            profiles::with_context(profiles::context_for("alice"), || subscriber("shared"));
        let old_observed = observed.clone();
        outgoing.deliver = Box::new(move |_, delivery| {
            old_observed.lock().unwrap().push(profiles::current_id());
            drop(delivery);
        });
        let mut selected =
            profiles::with_context(profiles::context_for("bob"), || subscriber("shared"));
        let new_observed = observed.clone();
        let keep = retained.clone();
        selected.deliver = Box::new(move |_, delivery| {
            new_observed.lock().unwrap().push(profiles::current_id());
            *keep.lock().unwrap() = Some(delivery);
        });
        let subscribers = {
            let mut queue = queue.lock().unwrap();
            assert!(queue.add(outgoing));
            assert!(queue.add(selected));
            for url in ["b", "c", "pending"] {
                assert!(queue.add(subscriber(url)));
            }
            let first = queue.next().unwrap();
            let second = queue.next().unwrap();
            assert_ne!(first.profile, second.profile);
            for _ in 2..WORKERS {
                assert!(queue.next().is_some());
            }
            assert!(queue.next().is_none());
            [
                queue.work.remove(&first).unwrap().subscribers,
                queue.work.remove(&second).unwrap().subscribers,
            ]
        };
        for subscribers in subscribers {
            assert_eq!(
                subscribers.len(),
                1,
                "different owners cannot share a transport"
            );
            let release_queue = queue.clone();
            let acknowledgement =
                CompletionAck::new(move || release_queue.lock().unwrap().active -= 1);
            for subscriber in subscribers {
                subscriber.dispatch(Delivery {
                    pixels: None,
                    _acknowledgement: acknowledgement.clone(),
                });
            }
            drop(acknowledgement);
        }
        assert_eq!(*observed.lock().unwrap(), vec!["alice", "bob"]);
        assert_eq!(profiles::capture(), original_context);
        assert_eq!(queue.lock().unwrap().next().unwrap().url, "pending");
        assert!(queue.lock().unwrap().add(subscriber("last")));
        assert!(
            queue.lock().unwrap().next().is_none(),
            "Bob still owns its delivery lane"
        );
        drop(retained.lock().unwrap().take());
        assert_eq!(queue.lock().unwrap().next().unwrap().url, "last");
    }

    #[test]
    fn no_live_subscribers_release_the_completion_slot_without_ui_dispatch() {
        let active = Arc::new(AtomicUsize::new(1));
        let released = Arc::new(AtomicUsize::new(0));
        let count = active.clone();
        let releases = released.clone();
        let acknowledgement = CompletionAck::new(move || {
            count.fetch_sub(1, Ordering::SeqCst);
            releases.fetch_add(1, Ordering::SeqCst);
        });
        // With every subscriber cancelled the transport owns the sole guard;
        // no event-loop callback is needed to free its lane.
        let pixels = SharedPixelBuffer::<Rgba8Pixel>::new(1, 1);
        drop(pixels);
        drop(acknowledgement);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(released.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn queue_partitions_profile_activation_authority_and_failure_cooldown() {
        let mut queue = Queue::default();
        let mut a = subscriber("same-url");
        a.context = profiles::Context {
            id: "a".into(),
            epoch: 1,
            session: None,
        };
        let a_key = QueueIdentity {
            image: a.request.identity.clone(),
            profile: a.context.id.clone(),
            epoch: a.context.epoch,
            authority: a.owner.as_ref().ok().map(|owner| owner.generation()),
        };
        let mut b = subscriber("same-url");
        b.context = profiles::Context {
            id: "b".into(),
            epoch: 2,
            session: None,
        };
        queue.failed.insert(a_key.clone(), Instant::now());
        assert!(queue.add(b));
        assert!(!queue.add(a));
        queue.failed.clear();
        let mut a = subscriber("same-url");
        a.context = profiles::Context {
            id: a_key.profile.clone(),
            epoch: a_key.epoch,
            session: None,
        };
        assert!(queue.add(a));
        assert_eq!(
            queue.work.len(),
            2,
            "B must not join A's same-URL transport"
        );
        let mut b_again = subscriber("same-url");
        b_again.context = profiles::Context {
            id: "b".into(),
            epoch: 2,
            session: None,
        };
        assert!(queue.add(b_again));
        assert!(queue.work.values().any(|work| work.subscribers.len() == 2));
    }

    #[test]
    fn queued_b_artwork_keeps_inventory_when_a_completion_refills_global_queue() {
        const ENV: &str = "NOVA_PAGE_ARTWORK_OWNER_REFILL_TEST";
        if std::env::var_os(ENV).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::page_artwork::tests::queued_b_artwork_keeps_inventory_when_a_completion_refills_global_queue", "--nocapture"])
                .env(ENV, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use nova_providers::plugin_settings as settings;
        use std::io::{Cursor, Read, Write};
        struct Backend(&'static str);
        impl settings::PluginSettingsBackend for Backend {
            fn owner_id(&self) -> Option<String> {
                Some(self.0.into())
            }
            fn load_settings(&self) -> Result<Option<String>, nova_providers::ProviderHostError> {
                Ok(None)
            }
            fn save_settings(&self, _: &str) -> Result<(), nova_providers::ProviderHostError> {
                Ok(())
            }
            fn load_cache(&self) -> Result<Option<String>, nova_providers::ProviderHostError> {
                Ok(None)
            }
            fn save_cache(&self, _: &str) -> Result<(), nova_providers::ProviderHostError> {
                Ok(())
            }
        }
        fn package(label: &str, color: [u8; 4]) -> (Arc<nova_providers::AddonPackage>, Vec<u8>) {
            let mut png = Cursor::new(Vec::new());
            image::RgbaImage::from_pixel(2, 2, image::Rgba(color))
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            let png = png.into_inner();
            let specific = format!("only-{label}.png");
            let manifest = serde_json::json!({"formatVersion":1,"id":"pageownerfixture","name":label,"version":"1.0.0","entry":"index.js","permissions":{},"assets":["shared.png",specific]}).to_string();
            let addon =
                serde_json::json!({"id":"nova.pageownerfixture","name":label,"version":"1.0.0"})
                    .to_string();
            let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
            for (path, contents) in [
                ("manifest.json", manifest.as_bytes()),
                ("stremio-manifest.json", addon.as_bytes()),
                ("LICENSE", b"Fixture".as_slice()),
                ("NOTICE.md", b"Fixture".as_slice()),
                (
                    "index.js",
                    b"globalThis.novaProvider={handle(request){return request.manifest;}};"
                        .as_slice(),
                ),
                ("shared.png", png.as_slice()),
                (specific.as_str(), png.as_slice()),
            ] {
                archive
                    .start_file(
                        path,
                        zip::write::SimpleFileOptions::default()
                            .compression_method(zip::CompressionMethod::Stored),
                    )
                    .unwrap();
                archive.write_all(contents).unwrap();
            }
            (
                nova_providers::AddonPackage::parse(&archive.finish().unwrap().into_inner())
                    .unwrap(),
                png,
            )
        }
        assert!(
            !profiles::enabled(),
            "isolated scheduler fixture owns no selected UI session"
        );
        let mut cache = active_cache_settings();
        cache.enabled = false;
        nova_config::set_cache_settings(cache);
        set_decoded_cache_budget(0);
        decoded_cache_clear();
        let temp = std::env::temp_dir().join(format!("nova-page-owner-{}", std::process::id()));
        std::fs::create_dir_all(&temp).unwrap();
        nova_storage::init_at(&temp);
        let directory = super::super::profile_session::SessionDirectory::new();
        let session = directory.prepare("fresh-image-owner", Shared::default());
        let capability = settings::owned_settings_capability(
            settings::prepare_plugin_settings(Arc::new(Backend("fresh-image-owner"))).unwrap(),
        );
        assert!(session.providers.set(capability.clone()).is_ok());
        let context = session.context();
        let before = capture_image_owner(&context).unwrap();
        capability
            .with(|| settings::set_setting("pageownerfixture", "route", "edited".into()).unwrap());
        assert!(!capability.is_current());
        let after = capture_image_owner(&context).unwrap();
        assert_ne!(
            before.generation(),
            after.generation(),
            "new image must refresh the session owner's settings revision"
        );
        let weak = Arc::downgrade(&session);
        let mut retired = subscriber("retired-failed-image");
        retired.context = context.clone();
        retired.owner = Ok(after);
        let mut failed = Queue::default();
        assert!(failed.add(retired));
        let key = failed.next().unwrap();
        drop(failed.work.remove(&key));
        failed.failed.insert(key, Instant::now());
        drop(context);
        drop(session);
        assert!(
            weak.upgrade().is_none(),
            "idle cooldown must not retain a retired profile session"
        );
        assert_eq!(
            failed.failed.len(),
            1,
            "weak release must not need another request/prune"
        );
        let (package_a, png_a) = package("A", [223, 11, 33, 255]);
        let (package_b, _) = package("B", [5, 127, 231, 255]);
        let inventory_a =
            nova_providers::OwnedProviderSnapshot::new(vec![package_a], Default::default(), vec![]);
        let inventory_b =
            nova_providers::OwnedProviderSnapshot::new(vec![package_b], Default::default(), vec![]);
        let settings_a = settings::owned_settings_capability(
            settings::prepare_plugin_settings(Arc::new(Backend("queue-owner-a"))).unwrap(),
        );
        let settings_b = settings::owned_settings_capability(
            settings::prepare_plugin_settings(Arc::new(Backend("queue-owner-b"))).unwrap(),
        );
        let owner_a = nova_media::net::ImageFetchOwner::new(settings_a, inventory_a);
        let owner_b = nova_media::net::ImageFetchOwner::new(settings_b, inventory_b);
        assert_ne!(owner_a.generation(), owner_b.generation());
        let context_a = profiles::Context {
            id: "a".into(),
            epoch: 1,
            session: None,
        };
        let context_b = profiles::Context {
            id: "b".into(),
            epoch: 2,
            session: None,
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut sockets = Vec::new();
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = [0; 2048];
                socket.read(&mut request).unwrap();
                sockets.push(socket);
                entered_tx.send(()).unwrap();
            }
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            for mut socket in sockets {
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", png_a.len()).unwrap();
                socket.write_all(&png_a).unwrap();
            }
        });
        *QUEUE.lock().unwrap() = Queue::default();
        let mut old_live = Vec::new();
        for index in 0..WORKERS {
            let mut a = subscriber(&format!("http://{address}/a-{index}.png"));
            a.context = context_a.clone();
            a.owner = Ok(owner_a.clone());
            old_live.push(a.request.live.clone());
            a.deliver = Box::new(|_, _| panic!("cancelled A subscriber must not publish"));
            assert!(QUEUE.lock().unwrap().add(a));
        }
        pump();
        for _ in 0..2 {
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert_eq!(QUEUE.lock().unwrap().active, WORKERS);
        for live in old_live {
            live.store(false, Ordering::Release);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        for path in ["only-B.png", "shared.png", "shared.png"] {
            let mut b = subscriber(&format!("nova-provider://pageownerfixture/{path}"));
            b.context = context_b.clone();
            b.owner = Ok(owner_b.clone());
            let out = tx.clone();
            b.deliver = Box::new(move |request, delivery| {
                let pixels = delivery
                    .pixels
                    .as_ref()
                    .expect("B asset must resolve under B inventory");
                out.send((request.identity.url, pixels.as_bytes()[..4].to_vec()))
                    .unwrap();
            });
            assert!(QUEUE.lock().unwrap().add(b));
        }
        // No B transport can launch until an A completion drops its last ack.
        assert_eq!(QUEUE.lock().unwrap().pending.len(), 2);
        release_tx.send(()).unwrap();
        for _ in 0..3 {
            let (_, pixel) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
            assert_eq!(pixel, [5, 127, 231, 255]);
        }
        server.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let queue = QUEUE.lock().unwrap();
            if queue.active == 0 && queue.work.is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "ownership regression stranded a page lane"
            );
            drop(queue);
            std::thread::yield_now();
        }
        assert_eq!(decoded_cache_stats(), (0, 0));
    }

    #[test]
    fn queue_deduplicates_caps_workers_and_skips_cancelled_or_cooling_work() {
        let mut queue = Queue::default();
        queue
            .failed
            .insert(queue_identity("failed"), Instant::now());
        assert!(!queue.add(subscriber("failed")));
        let cancelled = subscriber("cancelled");
        cancelled.request.live.store(false, Ordering::Release);
        assert!(queue.add(cancelled));
        for url in ["a", "a", "b", "c", "d", "e"] {
            assert!(queue.add(subscriber(url)));
        }
        assert_eq!(queue.work[&queue_identity("a")].subscribers.len(), 2);
        for expected in ["a", "b", "c", "d"] {
            assert_eq!(queue.next().unwrap().url, expected);
        }
        assert!(queue.next().is_none());
        assert_eq!(queue.active, WORKERS);
        assert!(!queue.work.contains_key(&queue_identity("cancelled")));
        queue.active -= 1;
        assert_eq!(queue.next().unwrap().url, "e");
    }
}

#[cfg(test)]
mod pump_guard_tests {
    use super::with_pump_guard;
    use std::{cell::Cell, rc::Rc};

    #[test]
    fn deep_synchronous_rejection_with_closed_dispatch_drains_without_recursion() {
        struct Pending {
            jobs: Cell<usize>,
            active: Cell<usize>,
            completed: Cell<usize>,
            depth: Cell<usize>,
            peak_depth: Cell<usize>,
        }
        fn pump(pending: Rc<Pending>) {
            thread_local! { static RUNNING: Cell<bool> = const { Cell::new(false) }; }
            pending.depth.set(pending.depth.get() + 1);
            pending
                .peak_depth
                .set(pending.peak_depth.get().max(pending.depth.get()));
            with_pump_guard(&RUNNING, || {
                while pending.jobs.get() != 0 {
                    pending.jobs.set(pending.jobs.get() - 1);
                    pending.active.set(pending.active.get() + 1);
                    // Model the actual closed-dispatch callback: release its
                    // lane and synchronously request another pump.
                    pending.completed.set(pending.completed.get() + 1);
                    pending.active.set(pending.active.get() - 1);
                    pump(pending.clone());
                }
            });
            pending.depth.set(pending.depth.get() - 1);
        }
        let pending = Rc::new(Pending {
            jobs: Cell::new(10_000),
            active: Cell::new(0),
            completed: Cell::new(0),
            depth: Cell::new(0),
            peak_depth: Cell::new(0),
        });
        pump(pending.clone());
        assert_eq!(
            (
                pending.jobs.get(),
                pending.active.get(),
                pending.completed.get()
            ),
            (0, 0, 10_000)
        );
        assert_eq!(pending.peak_depth.get(), 2);
        // An accepted asynchronous request must not leave the guard latched.
        pending.jobs.set(1);
        pump(pending.clone());
        assert_eq!(pending.completed.get(), 10_001);
    }

    #[test]
    fn pump_guard_resets_after_panic_and_does_not_exclude_other_threads() {
        thread_local! { static RUNNING: Cell<bool> = const { Cell::new(false) }; }
        assert!(
            std::panic::catch_unwind(|| with_pump_guard(&RUNNING, || panic!(
                "injected pump panic"
            )))
            .is_err()
        );
        let ran = Cell::new(false);
        with_pump_guard(&RUNNING, || ran.set(true));
        assert!(ran.get());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    with_pump_guard(&RUNNING, || {
                        barrier.wait();
                    });
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    }
}

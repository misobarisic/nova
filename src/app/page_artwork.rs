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
    deliver: Box<dyn FnOnce(Request, Delivery) + Send>,
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
struct Work {
    identity: Identity,
    subscribers: Vec<Subscriber>,
}
#[derive(Default)]
struct Queue {
    work: HashMap<Identity, Work>,
    pending: VecDeque<Identity>,
    failed: HashMap<Identity, Instant>,
    active: usize,
}
static QUEUE: LazyLock<Mutex<Queue>> = LazyLock::new(|| Mutex::new(Queue::default()));

impl Queue {
    fn add(&mut self, subscriber: Subscriber) -> bool {
        let identity = subscriber.request.identity.clone();
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
                    subscribers: vec![subscriber],
                },
            );
        }
        true
    }
    fn next(&mut self) -> Option<Identity> {
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
fn pump() {
    loop {
        let Some(identity) = QUEUE.lock().unwrap().next() else {
            return;
        };
        let worker_revision = image_settings_revision();
        net::fetch_image(identity.url.clone(), identity.image_use, move |pixels| {
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
                (subscriber.deliver)(
                    subscriber.request,
                    Delivery {
                        pixels: delivered,
                        _acknowledgement: acknowledgement.clone(),
                    },
                );
            }
            // Clones share one backing buffer. Drop the transport's owner
            // before the final acknowledgement can admit another worker.
            drop(pixels);
            drop(acknowledgement);
        });
    }
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
        let keys: Vec<_> = match page {
            Page::Calendar => app
                .get_calendar_rows()
                .iter()
                .map(|row| format!("r{}", row.key))
                .chain(
                    app.get_calendar_later()
                        .iter()
                        .map(|row| format!("l{}", row.key)),
                )
                .collect(),
            Page::Downloads => app
                .get_downloads_page_groups()
                .iter()
                .map(|row| row.id.to_string())
                .collect(),
        };
        for key in keys {
            paint(&app, page, &key, None, "");
        }
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
            let _ = slint::invoke_from_event_loop(move || {
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
        if !QUEUE.lock().unwrap().add(Subscriber { request, deliver }) {
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

    fn subscriber(url: &str) -> Subscriber {
        Subscriber {
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
    fn queue_deduplicates_caps_workers_and_skips_cancelled_or_cooling_work() {
        let mut queue = Queue::default();
        queue.failed.insert(identity("failed"), Instant::now());
        assert!(!queue.add(subscriber("failed")));
        let cancelled = subscriber("cancelled");
        cancelled.request.live.store(false, Ordering::Release);
        assert!(queue.add(cancelled));
        for url in ["a", "a", "b", "c", "d", "e"] {
            assert!(queue.add(subscriber(url)));
        }
        assert_eq!(queue.work[&identity("a")].subscribers.len(), 2);
        for expected in ["a", "b", "c", "d"] {
            assert_eq!(queue.next().unwrap().url, expected);
        }
        assert!(queue.next().is_none());
        assert_eq!(queue.active, WORKERS);
        assert!(!queue.work.contains_key(&identity("cancelled")));
        queue.active -= 1;
        assert_eq!(queue.next().unwrap().url, "e");
    }
}

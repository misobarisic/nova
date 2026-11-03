//! Library release planner. UI state is local; scheduled identities are never row indices.
#[cfg(test)]
use super::home::next_upcoming_episode;
use super::home::{cal_cells, civil_from_days, days_from_civil, month_first, weekday_monday0};
use super::*;
use crate::{CalendarDay, CalendarRelease};
use chrono::Datelike;

/// UI dates follow the device's civil day. Provider release-day comparisons
/// continue to use their existing UTC-based anchor in `episodes::today_days`.
fn calendar_day_at<Tz: chrono::TimeZone>(now: chrono::DateTime<Tz>) -> i64 {
    let date = now.date_naive();
    days_from_civil(i64::from(date.year()), date.month(), date.day())
}

fn calendar_today_days() -> i64 {
    calendar_day_at(chrono::Local::now())
}

type CalendarKey = (String, String);
type OwnerKey = (String, bool);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Versions {
    metadata: u64,
    facts: u64,
    policy: u64,
}
#[derive(Default)]
struct OwnerChanges {
    versions: Versions,
    episodes: HashMap<CalendarKey, u64>,
}
static CHANGES: LazyLock<Mutex<HashMap<OwnerKey, OwnerChanges>>> = LazyLock::new(Mutex::default);
static NEXT_CHANGE: AtomicU64 = AtomicU64::new(1);
static WAKE_QUEUED: AtomicBool = AtomicBool::new(false);

/// Successful commits advance receipts synchronously. This hook never takes
/// Shared or the sync owner; writers can call it while holding their own locks.
pub(super) fn snapshot_committed(owner: &profiles::ProfileStore, key: &str) {
    let episode = key
        .strip_prefix("episodes:")
        .and_then(|key| key.split_once('\u{1}'));
    if episode.is_none()
        && !key.starts_with("meta_header:")
        && !matches!(key, "library" | "settings" | profile_policy::KEY)
    {
        return;
    }
    let version = NEXT_CHANGE.fetch_add(1, Ordering::AcqRel);
    {
        let mut changes = CHANGES.lock().unwrap();
        let changes = changes
            .entry((owner.id().into(), owner.is_scoped()))
            .or_default();
        if let Some((kind, id)) = episode {
            changes.versions.metadata = version;
            changes.episodes.insert((kind.into(), id.into()), version);
        } else if key == profile_policy::KEY {
            changes.versions.policy = version;
        } else {
            changes.versions.facts = version;
        }
    }
    invalidate();
}

/// Coalesce arbitrary worker/write notifications without taking UI state under
/// a storage lock. Delivery enters the selected activation, not the writer's.
pub(super) fn invalidate() {
    if WAKE_QUEUED.swap(true, Ordering::AcqRel) {
        return;
    }
    if slint::invoke_from_event_loop(|| {
        WAKE_QUEUED.store(false, Ordering::Release);
        if let Some(bridge) = bridge::with_global_bridge(Clone::clone) {
            profiles::with_context(profiles::active_context(), || bridge.calendar_tick());
        }
    })
    .is_err()
    {
        WAKE_QUEUED.store(false, Ordering::Release);
    }
}
fn versions(owner: &profiles::ProfileStore) -> Versions {
    CHANGES
        .lock()
        .unwrap()
        .get(&(owner.id().into(), owner.is_scoped()))
        .map(|changes| changes.versions)
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Receipt {
    profile: (String, u64),
    scoped: bool,
    versions: Versions,
    provider: u64,
    release_day: i64,
}
impl Receipt {
    fn capture(context: &profiles::Context, owner: &profiles::ProfileStore) -> Self {
        Self {
            profile: (context.id.clone(), context.epoch),
            scoped: owner.is_scoped(),
            versions: versions(owner),
            provider: nova_providers::metadata_revision(),
            release_day: today_days(),
        }
    }
    fn current(&self, context: &profiles::Context, owner: &profiles::ProfileStore) -> bool {
        profiles::is_current(context) && *self == Self::capture(context, owner)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TitleFacts {
    key: CalendarKey,
    name: String,
    poster_url: String,
    paused: bool,
    allowed: bool,
    include_specials: bool,
    scope: nova_config::progress::TitleProgress,
    watched: Vec<bool>,
}
impl TitleFacts {
    fn includes(&self, video: &Video) -> bool {
        (self.include_specials || video.season != Some(0))
            && (self.scope.included_episodes.contains(&video.id)
                || (!self.scope.excluded_episodes.contains(&video.id)
                    && !video
                        .season
                        .is_some_and(|season| self.scope.excluded_seasons.contains(&season))))
    }
}
#[derive(Clone)]
struct CachedMetadata {
    version: u64,
    videos: Arc<Vec<Video>>,
    failed: bool,
}
struct InputTitle {
    facts: TitleFacts,
    videos: Arc<Vec<Video>>,
}
struct CalendarTitle {
    key: CalendarKey,
    name: String,
    folded_name: String,
    poster_url: String,
}
struct Release {
    title: usize,
    episode_id: String,
    air_days: i64,
    absolute_date: Option<(i64, i64, i64)>,
    label: String,
    badge: String,
    se_label: String,
}
struct CalendarView {
    indices: Vec<usize>,
    by_day: std::collections::BTreeMap<i64, std::ops::Range<usize>>,
}
struct CalendarProjection {
    titles: Vec<CalendarTitle>,
    releases: Vec<Release>,
    all: CalendarView,
    next: CalendarView,
}
fn index_sorted(indices: Vec<usize>, releases: &[Release]) -> CalendarView {
    let mut by_day: std::collections::BTreeMap<i64, std::ops::Range<usize>> =
        std::collections::BTreeMap::new();
    for (position, index) in indices.iter().enumerate() {
        by_day
            .entry(releases[*index].air_days)
            .or_insert(position..position)
            .end = position + 1;
    }
    CalendarView { indices, by_day }
}
fn sorted_view(
    mut indices: Vec<usize>,
    releases: &[Release],
    titles: &[CalendarTitle],
) -> CalendarView {
    // Keep the baseline stable ordering for equal IDs across native types.
    indices.sort_by(|a, b| {
        let a = &releases[*a];
        let b = &releases[*b];
        (a.air_days, &titles[a.title].key.1, &a.episode_id).cmp(&(
            b.air_days,
            &titles[b.title].key.1,
            &b.episode_id,
        ))
    });
    index_sorted(indices, releases)
}

/// Pure: all release selection, names and row facts use captured inputs only.
/// Both scopes are retained, so changing scope/query/date never reopens caches.
fn build_calendar_projection(input: &[InputTitle], release_today: i64) -> CalendarProjection {
    let mut titles = Vec::new();
    let mut releases: Vec<Release> = Vec::new();
    let mut next = Vec::new();
    for input in input {
        let facts = &input.facts;
        if !facts.allowed || facts.paused || facts.key.0 == "movie" {
            continue;
        }
        let title = titles.len();
        titles.push(CalendarTitle {
            key: facts.key.clone(),
            name: facts.name.clone(),
            folded_name: facts.name.to_lowercase(),
            poster_url: facts.poster_url.trim().into(),
        });
        // The legacy row join selects the first native episode ID, even if
        // an addon repeats it later with a different schedule date.
        let mut drafts = HashMap::new();
        for video in input.videos.iter() {
            drafts.entry(video.id.as_str()).or_insert(video);
        }
        let mut available = 0;
        let mut watched = 0;
        let mut future: Option<(usize, u32, u32)> = None;
        for (position, video) in input.videos.iter().enumerate() {
            if !facts.includes(video) {
                continue;
            }
            let Some(air_days) = video.released.as_deref().and_then(iso_days) else {
                continue;
            };
            let index = releases.len();
            if air_days <= release_today {
                available += 1;
                watched += usize::from(facts.watched.get(position).copied().unwrap_or(false));
            } else if future.is_none_or(|(old, season, number)| {
                (
                    air_days,
                    video.season.unwrap_or(0),
                    video.episode.unwrap_or(0),
                ) < (releases[old].air_days, season, number)
            }) {
                future = Some((index, video.season.unwrap_or(0), video.episode.unwrap_or(0)));
            }
            let draft = drafts[video.id.as_str()];
            releases.push(Release {
                title,
                episode_id: video.id.clone(),
                air_days,
                absolute_date: draft.released.as_deref().and_then(iso_date),
                label: draft
                    .label()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                badge: episode_badge(draft),
                se_label: episode_se_label(draft),
            });
        }
        if available > 0
            && available == watched
            && let Some((index, _, _)) = future
        {
            next.push(index);
        }
    }
    let all = sorted_view((0..releases.len()).collect(), &releases, &titles);
    let next = sorted_view(next, &releases, &titles);
    CalendarProjection {
        titles,
        releases,
        all,
        next,
    }
}
impl CalendarProjection {
    fn filtered(&self, all: bool, query: &str) -> CalendarView {
        let matches: Vec<_> = self
            .titles
            .iter()
            .map(|title| title.folded_name.contains(query.trim()))
            .collect();
        let source = if all { &self.all } else { &self.next };
        index_sorted(
            source
                .indices
                .iter()
                .copied()
                .filter(|index| matches[self.releases[*index].title])
                .collect(),
            &self.releases,
        )
    }
}
impl CalendarView {
    fn agenda(
        &self,
        projection: &CalendarProjection,
        day: i64,
        before: usize,
        after: usize,
    ) -> std::ops::Range<usize> {
        let anchor = self
            .indices
            .partition_point(|index| projection.releases[*index].air_days < day);
        anchor.saturating_sub(before)..anchor.saturating_add(after).min(self.indices.len())
    }
    fn day(&self, day: i64) -> std::ops::Range<usize> {
        self.by_day.get(&day).cloned().unwrap_or(0..0)
    }
}
struct Cache {
    receipt: Receipt,
    progress: u64,
    facts: Vec<TitleFacts>,
    metadata: HashMap<CalendarKey, CachedMetadata>,
    policy: Arc<Result<profile_policy::TitlePolicySnapshot, String>>,
    projection: Arc<CalendarProjection>,
    failed: bool,
}
#[derive(Clone, PartialEq)]
struct RenderStamp {
    projection: usize,
    all: bool,
    query: String,
    first: i64,
    day: i64,
    mode: i32,
    before: usize,
    after: usize,
    local_day: i64,
    language: Language,
    spoiler: bool,
    reveals: u64,
    images: u64,
    artwork_allowed: bool,
}
#[derive(Default)]
pub(super) struct State {
    generation: u64,
    in_flight: Option<(u64, Receipt)>,
    cache: Option<Arc<Cache>>,
    view: Option<(usize, bool, String, Arc<CalendarView>)>,
    rendered: Option<RenderStamp>,
    retry_failed: bool,
    #[cfg(test)]
    captures: usize,
    #[cfg(test)]
    metadata_reads: usize,
    #[cfg(test)]
    policy_reads: usize,
    #[cfg(test)]
    builds: usize,
}
struct Request {
    bridge: Bridge,
    context: profiles::Context,
    owner: profiles::ProfileStore,
    generation: u64,
    receipt: Receipt,
    before: Option<Arc<Cache>>,
    retry: bool,
}
struct Completion {
    request: Request,
    cache: Arc<Cache>,
    #[cfg(test)]
    metadata_reads: usize,
    #[cfg(test)]
    policy_reads: usize,
    #[cfg(test)]
    built: bool,
}
#[derive(Default)]
struct Work {
    latest: Option<Request>,
    ready: Option<Completion>,
    publication_queued: bool,
}
static WORK: LazyLock<(Mutex<Work>, std::sync::Condvar)> =
    LazyLock::new(|| (Mutex::default(), std::sync::Condvar::new()));
static WORKER_STARTED: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static METADATA_READ_DELAY_MS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static METADATA_READ_BARRIER: Mutex<
    Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
> = Mutex::new(None);

fn request_current(request: &Request) -> bool {
    if !request.receipt.current(&request.context, &request.owner) {
        return false;
    }
    request
        .bridge
        .shared
        .lock()
        .unwrap()
        .calendar
        .in_flight
        .as_ref()
        .is_some_and(|(generation, receipt)| {
            *generation == request.generation && *receipt == request.receipt
        })
}
fn queue(request: Request) -> Result<(), String> {
    if !WORKER_STARTED.swap(true, Ordering::AcqRel)
        && let Err(error) = std::thread::Builder::new()
            .name("nova-calendar".into())
            .spawn(worker)
    {
        WORKER_STARTED.store(false, Ordering::Release);
        return Err(error.to_string());
    }
    let (work, ready) = &*WORK;
    work.lock().unwrap().latest = Some(request);
    ready.notify_one();
    Ok(())
}
fn worker() {
    loop {
        let request = {
            let (work, ready) = &*WORK;
            let mut work = work.lock().unwrap();
            while work.latest.is_none() {
                work = ready.wait(work).unwrap();
            }
            work.latest.take().unwrap()
        };
        let context = request.context.clone();
        let completion = profiles::with_context(context, || prepare_request(request));
        if let Some(completion) = completion {
            let enqueue = {
                let mut work = WORK.0.lock().unwrap();
                work.ready = Some(completion);
                if work.publication_queued {
                    false
                } else {
                    work.publication_queued = true;
                    true
                }
            };
            // This callback drains the latest result across activations. A
            // captured old-context wrapper must not strand its queued flag.
            if enqueue && slint::invoke_from_event_loop(publish_ready).is_err() {
                let mut work = WORK.0.lock().unwrap();
                work.publication_queued = false;
                work.ready = None;
            }
        }
    }
}
fn prepare_request(request: Request) -> Option<Completion> {
    if !request_current(&request) {
        return None;
    }
    let episode_versions = CHANGES
        .lock()
        .unwrap()
        .get(&(request.owner.id().into(), request.owner.is_scoped()))
        .map(|changes| changes.episodes.clone())
        .unwrap_or_default();
    let before = request.before.as_ref();
    let refresh_policy = request.retry
        || before.is_none_or(|before| {
            before.receipt.versions.policy != request.receipt.versions.policy
                || before.receipt.versions.facts != request.receipt.versions.facts
        });
    let policy = if refresh_policy {
        Arc::new(profile_policy::TitlePolicySnapshot::capture(&request.owner))
    } else {
        before.unwrap().policy.clone()
    };
    let seeds = {
        let state = request.bridge.shared.lock().unwrap();
        state
            .entries
            .iter()
            .filter(|entry| entry.type_ != "movie")
            .map(|entry| {
                let scope = state.cache_settings.progress_policy(&entry.id);
                TitleFacts {
                    key: (entry.type_.clone(), entry.id.clone()),
                    name: entry.name.clone(),
                    poster_url: entry.poster_url.clone(),
                    paused: matches!(
                        entry.watch_status,
                        WatchStatus::OnHold | WatchStatus::Dropped
                    ),
                    allowed: policy
                        .as_ref()
                        .as_ref()
                        .is_ok_and(|policy| policy.allows(&entry.type_, &entry.id)),
                    include_specials: scope.include_specials,
                    scope: scope.title,
                    watched: Vec::new(),
                }
            })
            .collect::<Vec<_>>()
    };
    let mut metadata = HashMap::new();
    let mut input = Vec::new();
    let mut failed = policy.is_err();
    #[cfg(test)]
    let mut metadata_reads = 0;
    for mut facts in seeds {
        if !request_current(&request) {
            return None;
        }
        let version = episode_versions
            .get(&facts.key)
            .copied()
            .unwrap_or_default();
        let saved = before
            .filter(|before| before.receipt.provider == request.receipt.provider)
            .and_then(|before| before.metadata.get(&facts.key))
            .filter(|saved| saved.version == version && !(request.retry && saved.failed));
        if !facts.allowed || facts.paused {
            input.push(InputTitle {
                facts,
                videos: Arc::new(Vec::new()),
            });
            continue;
        }
        let cached = if let Some(saved) = saved {
            saved.clone()
        } else {
            #[cfg(test)]
            {
                metadata_reads += 1;
                let barrier = METADATA_READ_BARRIER.lock().unwrap().take();
                if let Some((entered, release)) = barrier {
                    let _ = entered.send(());
                    let _ = release.recv();
                }

                std::thread::sleep(Duration::from_millis(
                    METADATA_READ_DELAY_MS.load(Ordering::Acquire),
                ));
            }
            match io::read_json_result::<Vec<Video>>(&episodes_key(&facts.key.0, &facts.key.1)) {
                Ok(videos) => CachedMetadata {
                    version,
                    videos: Arc::new(videos.unwrap_or_default()),
                    failed: false,
                },
                Err(error) => {
                    storage::report(error);
                    CachedMetadata {
                        version,
                        videos: Arc::new(Vec::new()),
                        failed: true,
                    }
                }
            }
        };
        failed |= cached.failed;
        metadata.insert(facts.key.clone(), cached.clone());
        input.push(InputTitle {
            facts: {
                facts.watched = Vec::new();
                facts
            },
            videos: cached.videos,
        });
    }
    // Copy booleans for known native targets, never EpisodeProgress/history.
    // Position checkpoints can advance the broad revision while metadata is
    // loading; they do not cancel it or force another metadata read.
    let progress = progress_projection::revision();
    for title in &mut input {
        let state = request.bridge.shared.lock().unwrap();
        title.facts.watched = title
            .videos
            .iter()
            .map(|video| {
                state
                    .progress
                    .get(&progress_map_key(&title.facts.key.1, &video.id))
                    .is_some_and(|record| record.watched)
            })
            .collect();
    }
    if !request_current(&request) {
        return None;
    }
    let facts = input
        .iter()
        .map(|title| title.facts.clone())
        .collect::<Vec<_>>();
    let reuse = before.filter(|before| {
        before.receipt.release_day == request.receipt.release_day
            && before.facts == facts
            && before.receipt.versions.metadata == request.receipt.versions.metadata
            && before.receipt.provider == request.receipt.provider
    });
    let projection = reuse
        .map(|before| before.projection.clone())
        .unwrap_or_else(|| {
            Arc::new(build_calendar_projection(
                &input,
                request.receipt.release_day,
            ))
        });
    let cache = Arc::new(Cache {
        receipt: request.receipt.clone(),
        progress,
        facts,
        metadata,
        policy,
        projection,
        failed,
    });
    Some(Completion {
        cache,
        #[cfg(test)]
        metadata_reads,
        #[cfg(test)]
        policy_reads: usize::from(refresh_policy),
        #[cfg(test)]
        built: reuse.is_none(),
        request,
    })
}
fn publish_ready() {
    let completion = {
        let mut work = WORK.0.lock().unwrap();
        work.publication_queued = false;
        work.ready.take()
    };
    let Some(completion) = completion else {
        return;
    };
    let context = completion.request.context.clone();
    if !profiles::is_current(&context) {
        return;
    }
    profiles::with_context(context, || {
        let request = &completion.request;
        if !request_current(request) {
            return;
        }
        {
            let mut state = request.bridge.shared.lock().unwrap();
            let calendar = &mut state.calendar;
            calendar.in_flight = None;
            // Even a position change after capture can retire publication.
            // Retain its metadata for the next semantic check, not its UI rows.
            calendar.cache = Some(completion.cache.clone());
            #[cfg(test)]
            {
                calendar.captures += 1;
                calendar.metadata_reads += completion.metadata_reads;
                calendar.policy_reads += completion.policy_reads;
                calendar.builds += usize::from(completion.built);
            }
        }
        request.bridge.apply_calendar_to_ui();
    });
}

fn retain_calendar_days(
    current: slint::ModelRc<CalendarDay>,
    rows: Vec<CalendarDay>,
) -> slint::ModelRc<CalendarDay> {
    if let Some(model) = current.as_any().downcast_ref::<VecModel<CalendarDay>>()
        && model.row_count() == rows.len()
        && rows.iter().enumerate().all(|(index, row)| {
            model
                .row_data(index)
                .is_some_and(|old| old.cell.epoch == row.cell.epoch)
        })
    {
        // Date selection/count updates keep their original controls and focus;
        // navigating to another date inventory still replaces the model.
        for (index, row) in rows.into_iter().enumerate() {
            if model.row_data(index).as_ref() != Some(&row) {
                model.set_row_data(index, row);
            }
        }
        current
    } else {
        Rc::new(VecModel::from(rows)).into()
    }
}

fn retain_calendar_rows(
    current: slint::ModelRc<CalendarRelease>,
    rows: Vec<CalendarRelease>,
) -> slint::ModelRc<CalendarRelease> {
    if let Some(model) = current.as_any().downcast_ref::<VecModel<CalendarRelease>>()
        && model.row_count() == rows.len()
        && rows
            .iter()
            .enumerate()
            .all(|(index, row)| model.row_data(index).is_some_and(|old| old.key == row.key))
    {
        for (index, row) in rows.into_iter().enumerate() {
            if model.row_data(index).as_ref() != Some(&row) {
                model.set_row_data(index, row);
            }
        }
        current
    } else {
        Rc::new(VecModel::from(rows)).into()
    }
}

/// Hold exactly the next actual metadata read; dropping the release sender
/// also unblocks the worker when a fixture assertion fails.
#[cfg(test)]
pub(super) fn metadata_barrier() -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (entered, observing) = std::sync::mpsc::channel();
    let (release, waiting) = std::sync::mpsc::channel();
    assert!(
        METADATA_READ_BARRIER
            .lock()
            .unwrap()
            .replace((entered, waiting))
            .is_none()
    );
    (observing, release)
}

#[cfg(test)]
pub(super) fn metadata_delay(milliseconds: u64) {
    METADATA_READ_DELAY_MS.store(milliseconds, Ordering::Release);
}

#[cfg(test)]
pub(super) fn counters(bridge: &Bridge) -> (usize, usize, usize, usize) {
    let state = bridge.shared.lock().unwrap();
    let calendar = &state.calendar;
    (
        calendar.captures,
        calendar.metadata_reads,
        calendar.policy_reads,
        calendar.builds,
    )
}

/// Test the real worker/publication path without depending on mock timer time.
#[cfg(test)]
pub(super) fn settle(bridge: &Bridge) {
    let bridge = bridge.clone();
    let settled = Arc::new(AtomicBool::new(false));
    let observed = settled.clone();
    let monitor = std::thread::spawn(move || {
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(20) {
            let done = {
                let state = bridge.shared.lock().unwrap();
                state.calendar.in_flight.is_none()
                    && state
                        .calendar
                        .cache
                        .as_ref()
                        .is_some_and(|cache| cache.progress == progress_projection::revision())
                    && state.calendar.rendered.is_some()
            };
            if done {
                observed.store(true, Ordering::Release);
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        slint::invoke_from_event_loop(|| slint::quit_event_loop().unwrap()).unwrap();
    });
    slint::run_event_loop().unwrap();
    monitor.join().unwrap();
    assert!(
        settled.load(Ordering::Acquire),
        "Calendar worker did not settle"
    );
}

impl Bridge {
    pub(super) fn show_downloads_page(&self) {
        self.shared
            .lock()
            .unwrap()
            .downloads_page_refresh
            .invalidate();
        if let Some(app) = self.app() {
            app.set_show_home(false);
            app.set_show_library(false);
            app.set_show_settings(false);
            app.set_show_about(false);
            app.set_show_calendar(false);
            app.set_show_downloads(true);
        }
        self.refresh_downloads_page();
    }

    pub(super) fn show_calendar_page(&self) {
        self.shared.lock().unwrap().calendar.retry_failed = true;
        self.refresh_release_reminders();
        if let Some(app) = self.app() {
            app.set_show_home(false);
            app.set_show_library(false);
            app.set_show_settings(false);
            app.set_show_about(false);
            app.set_show_downloads(false);
            app.set_show_calendar(true);
            if app.get_calendar_first() == 0 {
                let today = calendar_today_days();
                app.set_calendar_first(month_first(today) as i32);
                app.set_calendar_selected(today as i32);
            }
        }
        self.apply_calendar_to_ui();
    }

    pub(super) fn calendar_step(&self, delta: i32) {
        let Some(app) = self.app() else { return };
        let selected = i64::from(app.get_calendar_selected());
        if app.get_calendar_view() == 1 {
            let day = selected + i64::from(delta) * 7;
            app.set_calendar_selected(day as i32);
            app.set_calendar_first(month_first(day) as i32);
        } else if app.get_calendar_view() == 2 {
            return;
        } else {
            let (year, month, _) = civil_from_days(i64::from(app.get_calendar_first()));
            let shifted = i64::from(month) - 1 + i64::from(delta);
            let first = days_from_civil(
                year + shifted.div_euclid(12),
                (shifted.rem_euclid(12) + 1) as u32,
                1,
            );
            app.set_calendar_first(first as i32);
            app.set_calendar_selected(first as i32);
        }
        self.apply_calendar_to_ui();
    }

    pub(super) fn calendar_today(&self) {
        if let Some(app) = self.app() {
            let today = calendar_today_days();
            app.set_calendar_first(month_first(today) as i32);
            app.set_calendar_selected(today as i32);
        }
        self.calendar_reset();
    }

    /// Search, scope and view changes start a new bounded agenda window.
    pub(super) fn calendar_reset(&self) {
        if let Some(app) = self.app() {
            app.set_calendar_agenda_before(0);
            app.set_calendar_agenda_after(AGENDA_BATCH as i32);
            app.set_calendar_agenda_revision(app.get_calendar_agenda_revision().wrapping_add(1));
        }
        self.apply_calendar_to_ui();
    }

    pub(super) fn calendar_agenda_more(&self, direction: i32) {
        let Some(app) = self.app() else { return };
        if !app.get_show_calendar() || app.get_calendar_view() != 2 {
            return;
        }
        if direction < 0 && app.get_calendar_has_earlier() {
            app.set_calendar_agenda_before(
                app.get_calendar_agenda_before()
                    .saturating_add(AGENDA_BATCH as i32),
            );
        } else if direction > 0 && app.get_calendar_has_later() {
            app.set_calendar_agenda_after(
                app.get_calendar_agenda_after()
                    .saturating_add(AGENDA_BATCH as i32),
            );
        } else {
            return;
        }
        self.apply_calendar_to_ui();
    }

    pub(super) fn calendar_pick(&self, day: i32) {
        if let Some(app) = self.app() {
            app.set_calendar_selected(day);
        }
        self.apply_calendar_to_ui();
    }

    pub(super) fn calendar_release_picked(&self, key: SharedString) {
        let Ok((type_, series, episode)) = serde_json::from_str::<(String, String, String)>(&key)
        else {
            return;
        };
        let entry = self
            .shared
            .lock()
            .unwrap()
            .entries
            .iter()
            .find(|e| e.id == series && e.type_ == type_)
            .cloned();
        // Revalidate the episode against its series cache. A refreshed schedule
        // cannot redirect an old card to an unrelated row.
        if let Some(entry) = entry.filter(|_| {
            read_episodes_cache_for(&type_, &series)
                .is_some_and(|videos| videos.iter().any(|v| v.id == episode))
        }) {
            self.open_library_entry(entry);
            self.detail_tab_picked(3);
        }
    }

    /// Existing timer hook: inspect only in-memory receipts while visible.
    pub(super) fn calendar_tick(&self) {
        let Some(app) = self.app().filter(|app| app.get_show_calendar()) else {
            return;
        };
        let context = profiles::capture();
        let owner = profiles::ProfileStore::capture();
        let receipt = Receipt::capture(&context, &owner);
        let progress = progress_projection::revision();
        let stamp = self.calendar_render_stamp(&app, 0);
        let needs = {
            let state = self.shared.lock().unwrap();
            state
                .calendar
                .cache
                .as_ref()
                .is_none_or(|cache| cache.receipt != receipt || cache.progress != progress)
                || state.calendar.rendered.as_ref().is_none_or(|rendered| {
                    let mut stamp = stamp.clone();
                    stamp.projection = rendered.projection;
                    *rendered != stamp
                })
        };
        if needs {
            self.apply_calendar_to_ui();
        }
    }
    fn calendar_render_stamp(&self, app: &AppWindow, projection: usize) -> RenderStamp {
        let (language, spoiler) = {
            let state = self.shared.lock().unwrap();
            (
                state.cache_settings.language,
                state.cache_settings.spoiler_protection,
            )
        };
        RenderStamp {
            projection,
            all: app.get_calendar_all(),
            query: app.get_calendar_search().to_lowercase(),
            first: i64::from(app.get_calendar_first()),
            day: i64::from(app.get_calendar_selected()),
            mode: app.get_calendar_view(),
            before: app.get_calendar_agenda_before().max(0) as usize,
            after: app.get_calendar_agenda_after().max(1) as usize,
            local_day: calendar_today_days(),
            language,
            spoiler,
            reveals: spoiler::reveal_revision(),
            images: image_settings_revision(),
            artwork_allowed: app.get_page_artwork_allowed(),
        }
    }
    fn request_calendar(
        &self,
        context: profiles::Context,
        owner: profiles::ProfileStore,
        receipt: Receipt,
    ) {
        let request = {
            let mut state = self.shared.lock().unwrap();
            let calendar = &mut state.calendar;
            let retry =
                calendar.retry_failed && calendar.cache.as_ref().is_some_and(|cache| cache.failed);
            if !retry
                && calendar
                    .in_flight
                    .as_ref()
                    .is_some_and(|(_, wanted)| *wanted == receipt)
            {
                return;
            }
            calendar.retry_failed = false;
            calendar.generation = calendar.generation.wrapping_add(1);
            calendar.in_flight = Some((calendar.generation, receipt.clone()));
            Request {
                bridge: self.clone(),
                context,
                owner,
                generation: calendar.generation,
                receipt,
                before: calendar.cache.clone(),
                retry,
            }
        };
        if let Err(error) = queue(request) {
            self.shared.lock().unwrap().calendar.in_flight = None;
            tracing::warn!(%error, "start Calendar worker");
        }
    }
    pub(super) fn apply_calendar_to_ui(&self) {
        let Some(app) = self.app().filter(|app| app.get_show_calendar()) else {
            return;
        };
        let context = profiles::capture();
        let owner = profiles::ProfileStore::capture();
        let receipt = Receipt::capture(&context, &owner);
        let progress = progress_projection::revision();
        let (cache, retry) = {
            let mut state = self.shared.lock().unwrap();
            let retry = state.calendar.retry_failed
                && state
                    .calendar
                    .cache
                    .as_ref()
                    .is_some_and(|cache| cache.failed);
            if !retry {
                state.calendar.retry_failed = false;
            }
            (state.calendar.cache.clone(), retry)
        };
        if retry
            || cache
                .as_ref()
                .is_none_or(|cache| cache.receipt != receipt || cache.progress != progress)
        {
            // Never project data across a changed permission receipt. Keep the
            // old allocation only as worker input, not as a permission token.
            if cache.as_ref().is_none_or(|cache| {
                cache.receipt.profile != receipt.profile
                    || cache.receipt.versions.policy != receipt.versions.policy
                    || cache.receipt.versions.facts != receipt.versions.facts
            }) {
                app.set_calendar_rows(Rc::new(VecModel::<CalendarRelease>::default()).into());
                app.set_calendar_later(Rc::new(VecModel::<CalendarRelease>::default()).into());
                app.set_calendar_count(0);
                app.set_calendar_has_earlier(false);
                app.set_calendar_has_later(false);
                // Cell previews also contain title names; permission changes
                // must retire them along with release cards.
                let clear_cells = |model: slint::ModelRc<CalendarDay>| {
                    let rows = model
                        .iter()
                        .map(|mut row| {
                            row.preview = SharedString::default();
                            row.cell.count = 0;
                            row
                        })
                        .collect();
                    retain_calendar_days(model, rows)
                };
                app.set_calendar_cells(clear_cells(app.get_calendar_cells()));
                app.set_calendar_week(clear_cells(app.get_calendar_week()));
            }
            self.request_calendar(context, owner, receipt);
            return;
        }
        let cache = cache.unwrap();
        let projection = &cache.projection;
        let stamp = self.calendar_render_stamp(&app, Arc::as_ptr(projection) as usize);
        let view = {
            let mut state = self.shared.lock().unwrap();
            let calendar = &mut state.calendar;
            if calendar.rendered.as_ref() == Some(&stamp) {
                drop(state);
                self.schedule_page_artwork(super::page_artwork::Page::Calendar);
                return;
            }
            if let Some((identity, all, query, view)) = &calendar.view
                && *identity == stamp.projection
                && *all == stamp.all
                && *query == stamp.query
            {
                view.clone()
            } else {
                let view = Arc::new(projection.filtered(stamp.all, &stamp.query));
                calendar.view = Some((
                    stamp.projection,
                    stamp.all,
                    stamp.query.clone(),
                    view.clone(),
                ));
                view
            }
        };
        let counts: HashMap<_, _> = view
            .by_day
            .iter()
            .map(|(day, range)| (*day, range.len()))
            .collect();
        let weekdays = [
            text::tr("Mon"),
            text::tr("Tue"),
            text::tr("Wed"),
            text::tr("Thu"),
            text::tr("Fri"),
            text::tr("Sat"),
            text::tr("Sun"),
        ];
        let cells = cal_cells(stamp.first, &counts, Some(stamp.day), stamp.local_day)
            .into_iter()
            .map(|cell| {
                let preview = view.indices[view.day(i64::from(cell.epoch))]
                    .iter()
                    .take(2)
                    .map(|index| {
                        projection.titles[projection.releases[*index].title]
                            .name
                            .as_str()
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                CalendarDay {
                    weekday: weekdays[weekday_monday0(i64::from(cell.epoch)) as usize].into(),
                    cell,
                    preview: preview.into(),
                }
            })
            .collect::<Vec<_>>();
        let week_start = stamp.day - weekday_monday0(stamp.day) as i64;
        let week = (0..7)
            .map(|offset| {
                let epoch = week_start + offset;
                let (_, _, date) = civil_from_days(epoch);
                CalendarDay {
                    cell: CalCell {
                        epoch: epoch as i32,
                        day: date as i32,
                        in_month: true,
                        count: counts.get(&epoch).copied().unwrap_or(0) as i32,
                        selected: epoch == stamp.day,
                        today: epoch == stamp.local_day,
                    },
                    preview: SharedString::default(),
                    weekday: weekdays[offset as usize].into(),
                }
            })
            .collect::<Vec<_>>();
        let period = calendar_period(stamp.first, stamp.day, stamp.mode);
        let agenda = view.agenda(projection, stamp.day, stamp.before, stamp.after);
        let has_earlier = agenda.start > 0;
        let has_later = agenda.end < view.indices.len();
        let visible = if stamp.mode == 2 {
            agenda
        } else if stamp.mode == 0 {
            view.day(stamp.day)
        } else {
            view.indices
                .partition_point(|index| projection.releases[*index].air_days < period.0)
                ..view
                    .indices
                    .partition_point(|index| projection.releases[*index].air_days < period.1)
        };
        let later_start = view
            .indices
            .partition_point(|index| projection.releases[*index].air_days <= stamp.day);
        let later = later_start..later_start.saturating_add(8).min(view.indices.len());
        let (retained, watched) = {
            let state = self.shared.lock().unwrap();
            let watched: HashMap<_, _> = view.indices[visible.clone()]
                .iter()
                .chain(view.indices[later.clone()].iter())
                .map(|index| {
                    let row = &projection.releases[*index];
                    let title = &projection.titles[row.title];
                    (
                        *index,
                        state
                            .progress
                            .get(&progress_map_key(&title.key.1, &row.episode_id))
                            .is_some_and(|record| record.watched),
                    )
                })
                .collect();
            (
                state
                    .page_artwork
                    .retained_keys(super::page_artwork::Page::Calendar),
                watched,
            )
        };
        let reveals = spoiler::revealed_snapshot(&context);
        let old_rows: HashMap<_, _> = app
            .get_calendar_rows()
            .iter()
            .map(|row| (format!("r{}", row.key), row))
            .chain(
                app.get_calendar_later()
                    .iter()
                    .map(|row| (format!("l{}", row.key), row)),
            )
            .collect();
        let mut artwork_identities = Vec::new();
        let mut rows = |range: std::ops::Range<usize>, list: &str| {
            let mut previous = None;
            view.indices[range]
                .iter()
                .map(|index| {
                    let row = &projection.releases[*index];
                    let title = &projection.titles[row.title];
                    let group = if previous == Some(row.air_days) {
                        String::new()
                    } else {
                        calendar_date(row.air_days)
                    };
                    previous = Some(row.air_days);
                    let key = serde_json::to_string(&(&title.key.0, &title.key.1, &row.episode_id))
                        .unwrap_or_default();
                    let artwork_key = format!("{list}{key}");
                    let identity = super::page_artwork::Identity::new(
                        title.poster_url.clone(),
                        ImageUse::Fixed(Some(DISPLAY_POSTER_SIDE)),
                    );
                    let (poster, is_loaded) = if retained.contains(&artwork_key) {
                        if let Some(pixels) = decoded_cache_get(&identity.cache_key) {
                            (Image::from_rgba8(pixels), true)
                        } else if let Some(old) = old_rows
                            .get(&artwork_key)
                            .filter(|old| old.artwork_identity == identity.cache_key)
                        {
                            (old.poster.clone(), old.is_loaded)
                        } else {
                            (Image::default(), false)
                        }
                    } else {
                        (Image::default(), false)
                    };
                    let protected = stamp.spoiler
                        && !watched.get(index).copied().unwrap_or(false)
                        && !reveals.contains(&(
                            title.key.0.clone(),
                            title.key.1.clone(),
                            row.episode_id.clone(),
                        ));
                    let label = if !row.label.is_empty() {
                        row.label.as_str()
                    } else if !row.se_label.is_empty() {
                        row.se_label.as_str()
                    } else {
                        text::tr("Episode")
                    };
                    let subtitle = if protected {
                        row.se_label.clone()
                    } else {
                        format!("{} · {label}", row.badge)
                    };
                    let date = row
                        .absolute_date
                        .map(|(year, month, day)| {
                            text::absolute_date(day as u32, month as u32, year)
                        })
                        .unwrap_or_default();
                    let artwork_identity = identity.cache_key.clone().into();
                    artwork_identities.push((artwork_key, identity));
                    CalendarRelease {
                        key: key.into(),
                        artwork_identity,
                        title: title.name.clone().into(),
                        subtitle: subtitle.into(),
                        date: date.into(),
                        group: group.into(),
                        poster,
                        is_loaded,
                    }
                })
                .collect::<Vec<_>>()
        };
        let visible_rows = rows(visible, "r");
        let later_rows = rows(later, "l");
        if !receipt.current(&context, &owner)
            || progress_projection::revision() != cache.progress
            || spoiler::reveal_revision() != stamp.reveals
            || image_settings_revision() != stamp.images
        {
            return;
        }
        app.set_calendar_has_earlier(has_earlier);
        app.set_calendar_has_later(has_later);
        self.shared.lock().unwrap().page_artwork.reconcile(
            super::page_artwork::Page::Calendar,
            format!(
                "{}:{}:{}:{}:{}",
                stamp.first, stamp.day, stamp.mode, stamp.all, stamp.query
            ),
            artwork_identities,
            stamp.artwork_allowed,
        );
        app.set_calendar_rows(retain_calendar_rows(app.get_calendar_rows(), visible_rows));
        app.set_calendar_later(retain_calendar_rows(app.get_calendar_later(), later_rows));
        app.set_calendar_cells(retain_calendar_days(app.get_calendar_cells(), cells));
        app.set_calendar_week(retain_calendar_days(app.get_calendar_week(), week));
        let (year, month, _) = civil_from_days(stamp.first);
        app.set_calendar_title(
            if stamp.mode == 1 {
                text::calendar_week_title(
                    civil_from_days(week_start),
                    civil_from_days(week_start + 6),
                )
            } else if stamp.mode == 2 {
                text::tr("Timeline").into()
            } else {
                text::cal_month_title(month, year)
            }
            .into(),
        );
        app.set_calendar_day_title(calendar_date(stamp.day).into());
        app.set_calendar_count(if stamp.mode == 2 {
            view.indices.len()
        } else {
            view.by_day
                .range(period.0..period.1)
                .map(|(_, range)| range.len())
                .sum()
        } as i32);
        self.shared.lock().unwrap().calendar.rendered = Some(stamp);
        self.schedule_page_artwork(super::page_artwork::Page::Calendar);
        if app.get_show_calendar() {
            app.recover_back_focus();
        }
    }
}

const AGENDA_BATCH: usize = 60;

/// Page by release count, not calendar months: sparse schedules cross empty
/// months immediately, and dense dates cannot cause an unbounded first render.
#[cfg(test)]
fn agenda_window(
    schedule: &[UpcomingEntry],
    selected: i64,
    before: usize,
    after: usize,
) -> std::ops::Range<usize> {
    let anchor = schedule.partition_point(|item| item.air_days < selected);
    anchor.saturating_sub(before)..anchor.saturating_add(after).min(schedule.len())
}

fn calendar_date(day: i64) -> String {
    let (year, month, date) = civil_from_days(day);
    text::calendar_date(date, month, year)
}

/// Shift a local date by whole months, clamping dates such as January 31
/// to the last valid day of the destination month.
fn shift_month(day: i64, delta: i32) -> i64 {
    let (year, month, date) = civil_from_days(day);
    let shifted = i64::from(month) - 1 + i64::from(delta);
    let year = year + shifted.div_euclid(12);
    let month = (shifted.rem_euclid(12) + 1) as u32;
    let first = days_from_civil(year, month, 1);
    let next = if month == 12 {
        days_from_civil(year + 1, 1, 1)
    } else {
        days_from_civil(year, month + 1, 1)
    };
    first + i64::from(date).min(next - first) - 1
}

/// Half-open boundaries keep adjacent calendar periods free of duplicates.
fn calendar_period(first: i64, selected: i64, view: i32) -> (i64, i64) {
    match view {
        0 => (first, shift_month(first, 1)),
        1 => {
            let monday = selected - i64::from(weekday_monday0(selected));
            (monday, monday + 7)
        }
        _ => (i64::MIN, i64::MAX),
    }
}
#[cfg(test)]
fn in_period(day: i64, period: (i64, i64)) -> bool {
    day >= period.0 && day < period.1
}
#[cfg(test)]
fn calendar_releases(
    series_id: &str,
    videos: &[Video],
    all: bool,
    watched: impl Fn(&Video) -> bool,
    today: i64,
) -> Vec<(String, i64)> {
    let policy = nova_config::progress::active_progress_policy(series_id);
    if all {
        videos
            .iter()
            .filter(|v| policy.includes(&v.id, v.season))
            .filter_map(|v| {
                v.released
                    .as_deref()
                    .and_then(iso_days)
                    .map(|day| (v.id.clone(), day))
            })
            .collect()
    } else {
        next_upcoming_episode(series_id, videos, watched, today)
            .map(|(index, day)| vec![(videos[index].id.clone(), day)])
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(kind: &str, id: &str, name: &str, videos: Vec<Video>) -> InputTitle {
        let scope = nova_config::progress::active_progress_policy(id);
        InputTitle {
            facts: TitleFacts {
                key: (kind.into(), id.into()),
                name: name.into(),
                poster_url: String::new(),
                paused: false,
                allowed: true,
                include_specials: scope.include_specials,
                scope: scope.title,
                watched: vec![true; videos.len()],
            },
            videos: Arc::new(videos),
        }
    }
    fn release_keys(
        projection: &CalendarProjection,
        view: &CalendarView,
    ) -> Vec<(String, String, String, i64)> {
        view.indices
            .iter()
            .map(|index| {
                let row = &projection.releases[*index];
                let title = &projection.titles[row.title];
                (
                    title.key.0.clone(),
                    title.key.1.clone(),
                    row.episode_id.clone(),
                    row.air_days,
                )
            })
            .collect()
    }

    #[test]
    fn immutable_projection_matches_baseline_scopes_queries_dates_and_typed_ties() {
        let videos: Vec<Video> = serde_json::from_value(serde_json::json!([
            {"id":"past", "season":1,"episode":1,"released":"2026-01-01"},
            {"id":"z", "season":2,"episode":2,"released":"2026-11-03T23:00:00Z","title":"  A \n title  "},
            {"id":"a", "season":1,"episode":3,"released":"2026-11-03"},
            {"id":"same-rank", "season":1,"episode":3,"released":"2026-11-03"},
            {"id":"special", "season":0,"episode":1,"released":"2026-11-03"},
            {"id":"invalid", "released":"2026-02-30"}, {"id":"dateless"}
        ])).unwrap();
        let mut input = vec![
            input("series", "same", "MiXeD Alpha", videos.clone()),
            input("anime", "same", "Mixed Beta", videos.clone()),
            input("series", "hidden", "Mixed Hidden", videos.clone()),
            input("movie", "movie", "Mixed Movie", videos.clone()),
            input("series", "paused", "Mixed Paused", videos),
        ];
        input[2].facts.allowed = false;
        input[4].facts.paused = true;
        let today = date(2026, 11, 2);
        let projection = build_calendar_projection(&input, today);
        for all in [false, true] {
            for query in ["", "mixed", "alpha", " BETA ", "absent"] {
                let folded = query.to_lowercase();
                let mut baseline = Vec::new();
                for title in &input {
                    if !title.facts.allowed
                        || title.facts.paused
                        || title.facts.key.0 == "movie"
                        || !title.facts.name.to_lowercase().contains(folded.trim())
                    {
                        continue;
                    }
                    baseline.extend(
                        calendar_releases(&title.facts.key.1, &title.videos, all, |_| true, today)
                            .into_iter()
                            .map(|(episode, day)| {
                                (
                                    title.facts.key.0.clone(),
                                    title.facts.key.1.clone(),
                                    episode,
                                    day,
                                )
                            }),
                    );
                }
                baseline.sort_by(|a, b| (a.3, &a.1, &a.2).cmp(&(b.3, &b.1, &b.2)));
                let view = projection.filtered(all, &folded);
                assert_eq!(
                    release_keys(&projection, &view),
                    baseline,
                    "all={all}, query={query}"
                );
                for day in [today - 400, today, today + 1, today + 400] {
                    assert_eq!(
                        view.day(day).len(),
                        baseline.iter().filter(|row| row.3 == day).count()
                    );
                }
            }
        }
        input[0].facts.watched.fill(false);
        let blocked = build_calendar_projection(&input, today);
        assert_eq!(
            blocked.next.indices.len(),
            1,
            "an unwatched aired episode removes only its native title"
        );
        input[1].facts.scope.excluded_seasons.insert(1);
        input[1].facts.scope.included_episodes.insert("past".into());
        let scoped = build_calendar_projection(&input, today);
        let next = release_keys(&scoped, &scoped.next);
        assert_eq!(
            next[0].2, "z",
            "explicit inclusion precedes excluded season"
        );
    }

    #[test]
    fn immutable_projection_scale_1000_shows_100_episodes_has_indexed_bounded_windows() {
        let input: Vec<_> = (0..1000)
            .map(|show| {
                let videos = (0..100)
                    .map(|episode| {
                        let day = if episode < 90 {
                            date(2025, 1, 1) + episode
                        } else {
                            date(2027, 1, 1) + (show + episode) % 365
                        };
                        let (year, month, date) = civil_from_days(day);
                        Video {
                            id: episode.to_string(),
                            season: Some(1),
                            episode: Some(episode as u32),
                            released: Some(format!("{year:04}-{month:02}-{date:02}")),
                            ..Default::default()
                        }
                    })
                    .collect();
                input("series", &show.to_string(), &format!("Show {show}"), videos)
            })
            .collect();
        let projection = build_calendar_projection(&input, date(2026, 11, 2));
        assert_eq!(projection.all.indices.len(), 100_000);
        assert_eq!(projection.next.indices.len(), 1000);
        let future: usize = projection
            .all
            .by_day
            .range(date(2027, 1, 1)..)
            .map(|(_, range)| range.len())
            .sum();
        assert_eq!(future, 10_000);
        for offset in 0..1000 {
            let selected = date(2027, 1, 1) + offset % 365;
            assert!(
                projection
                    .all
                    .agenda(&projection, selected, 0, AGENDA_BATCH)
                    .len()
                    <= AGENDA_BATCH
            );
            let range = projection.all.day(selected);
            assert!(
                projection.all.indices[range]
                    .iter()
                    .all(|index| projection.releases[*index].air_days == selected)
            );
        }
    }

    #[test]
    fn date_selection_updates_retained_controls_and_new_month_replaces_them() {
        let day = |epoch, selected| CalendarDay {
            cell: crate::CalCell {
                epoch,
                selected,
                ..Default::default()
            },
            ..Default::default()
        };
        let first: slint::ModelRc<CalendarDay> =
            Rc::new(VecModel::from(vec![day(1, true), day(2, false)])).into();
        let selected = retain_calendar_days(first.clone(), vec![day(1, false), day(2, true)]);
        assert!(!first.row_data(0).unwrap().cell.selected);
        assert!(
            first.row_data(1).unwrap().cell.selected,
            "date selection must update the retained model rather than recreate the controls"
        );
        let next_month =
            retain_calendar_days(selected.clone(), vec![day(31, true), day(32, false)]);
        assert_eq!(selected.row_data(0).unwrap().cell.epoch, 1);
        assert_eq!(
            next_month.row_data(0).unwrap().cell.epoch,
            31,
            "a changed date inventory must not reuse mismatched actions"
        );
    }
    #[test]
    fn release_rows_retain_native_identity_and_update_content() {
        let row = |key: &str, title: &str| CalendarRelease {
            key: key.into(),
            title: title.into(),
            ..Default::default()
        };
        let original: slint::ModelRc<CalendarRelease> =
            Rc::new(VecModel::from(vec![row("series-id", "before")])).into();
        let retained = retain_calendar_rows(original.clone(), vec![row("series-id", "after")]);
        assert_eq!(original.row_data(0).unwrap().title, "after");
        let replacement = retain_calendar_rows(retained.clone(), vec![row("anime-id", "new")]);
        assert_eq!(retained.row_data(0).unwrap().key, "series-id");
        assert_eq!(replacement.row_data(0).unwrap().key, "anime-id");
    }
    fn date(year: i64, month: u32, day: u32) -> i64 {
        days_from_civil(year, month, day)
    }
    #[test]
    fn today_anchor_uses_local_civil_day_at_offset_month_and_year_boundaries() {
        for (instant, offset, expected) in [
            ("2026-11-01T00:30:00Z", -7 * 3600, date(2026, 10, 31)),
            ("2026-10-31T23:30:00Z", 2 * 3600, date(2026, 11, 1)),
            ("2027-01-01T00:30:00Z", -8 * 3600, date(2026, 12, 31)),
            ("2026-12-31T23:30:00Z", 14 * 3600, date(2027, 1, 1)),
        ] {
            let utc = chrono::DateTime::parse_from_rfc3339(instant).unwrap();
            let local = utc.with_timezone(&chrono::FixedOffset::east_opt(offset).unwrap());
            let today = calendar_day_at(local);
            assert_eq!(today, expected, "{instant} at {offset}");
            let (year, month, _) = civil_from_days(expected);
            let first = month_first(today);
            assert_eq!(first, date(year, month, 1));
            let cells = cal_cells(first, &HashMap::new(), Some(today), today);
            let marked = cells.iter().filter(|cell| cell.today).collect::<Vec<_>>();
            assert_eq!(marked.len(), 1);
            assert_eq!(i64::from(marked[0].epoch), expected);
            assert!(marked[0].selected);
        }
    }

    #[test]
    fn all_releases_include_past_dates_and_ignore_invalid_dates() {
        let videos: Vec<Video> = serde_json::from_value(serde_json::json!([
            {"id":"past", "released":"2026-09-12"},
            {"id":"future", "released":"2026-11-12T12:00:00Z"},
            {"id":"bad", "released":"2026-02-30"},
            {"id":"unknown"}
        ]))
        .unwrap();
        let today = date(2026, 10, 11);
        assert_eq!(
            calendar_releases("test", &videos, true, |_| false, today),
            vec![
                ("past".into(), date(2026, 9, 12)),
                ("future".into(), date(2026, 11, 12))
            ]
        );
        assert_eq!(
            calendar_releases("test", &videos, false, |_| true, today),
            vec![("future".into(), date(2026, 11, 12))]
        );
        assert!(calendar_releases("test", &videos, false, |_| false, today).is_empty());
    }
    #[test]
    fn month_steps_clamp_dates_across_leap_years_and_year_boundaries() {
        assert_eq!(shift_month(date(2026, 1, 31), 1), date(2026, 2, 28));
        assert_eq!(shift_month(date(2024, 1, 31), 1), date(2024, 2, 29));
        assert_eq!(shift_month(date(2026, 3, 31), -1), date(2026, 2, 28));
        assert_eq!(shift_month(date(2026, 1, 15), -1), date(2025, 12, 15));
        assert_eq!(shift_month(date(2026, 12, 15), 1), date(2027, 1, 15));
    }
    #[test]
    fn periods_have_inclusive_start_and_exclusive_end() {
        let selected = date(2026, 10, 11);
        let month = calendar_period(date(2026, 10, 1), selected, 0);
        assert_eq!(month, (date(2026, 10, 1), date(2026, 11, 1)));
        let week = calendar_period(date(2026, 10, 1), selected, 1);
        assert_eq!(week, (date(2026, 10, 5), date(2026, 10, 12)));
        for period in [month, week] {
            assert!(!in_period(period.0 - 1, period));
            assert!(in_period(period.0, period));
            assert!(in_period(period.1 - 1, period));
            assert!(!in_period(period.1, period));
        }
    }
    #[test]
    fn agenda_spans_months_and_years_in_bounded_nonduplicated_batches() {
        let selected = date(2026, 10, 15);
        let schedule = (0..200)
            .map(|index| UpcomingEntry {
                air_days: selected + (index - 80) * 40,
                episode_id: index.to_string(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let initial = agenda_window(&schedule, selected, 0, AGENDA_BATCH);
        assert_eq!(initial, 80..140);
        assert!(schedule[initial.end - 1].air_days > date(2027, 12, 31));
        let appended = agenda_window(&schedule, selected, 0, AGENDA_BATCH * 2);
        assert_eq!(appended, 80..200);
        let prepended = agenda_window(&schedule, selected, AGENDA_BATCH, AGENDA_BATCH * 2);
        assert_eq!(prepended, 20..200);
        assert_eq!(
            schedule[prepended.clone()]
                .iter()
                .map(|item| &item.episode_id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            prepended.len()
        );
        assert_eq!(
            agenda_window(&schedule, selected, usize::MAX, usize::MAX),
            0..200
        );
        assert_eq!(agenda_window(&[], selected, 60, 60), 0..0);
        assert_eq!(agenda_window(&schedule, date(9999, 1, 1), 0, 60), 200..200);
        assert_eq!(agenda_window(&schedule, date(9999, 1, 1), 60, 60), 140..200);
    }

    #[test]
    fn agenda_handles_dense_days_and_schedule_changes_without_index_state() {
        let selected = date(2026, 10, 15);
        let mut schedule = (0..130)
            .map(|index| UpcomingEntry {
                air_days: selected,
                episode_id: index.to_string(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        assert_eq!(agenda_window(&schedule, selected, 0, 60), 0..60);
        assert_eq!(agenda_window(&schedule, selected, 0, 120), 0..120);
        // Recompute the date anchor when metadata changes or a filter removes rows.
        schedule.insert(
            0,
            UpcomingEntry {
                air_days: selected - 30,
                ..Default::default()
            },
        );
        assert_eq!(agenda_window(&schedule, selected, 0, 60), 1..61);
        schedule.truncate(5);
        assert_eq!(agenda_window(&schedule, selected, 60, 120), 0..5);
    }
}

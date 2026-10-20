//! Coalesced, profile-owned Library/Home follow-up for explicit watch actions.
//! Only plain data crosses the worker boundary; the event loop publishes
//! already joined rows and never reopens metadata caches in this path.
use super::*;

static REVISION: AtomicU64 = AtomicU64::new(1);
#[cfg(test)]
static PUBLICATIONS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static PUBLICATION_MICROS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
pub(super) fn publication_count() -> u64 {
    PUBLICATIONS.load(Ordering::Acquire)
}

#[cfg(test)]
pub(super) fn last_publication_micros() -> u64 {
    PUBLICATION_MICROS.load(Ordering::Acquire)
}

pub(super) fn invalidate() {
    REVISION.fetch_add(1, Ordering::AcqRel);
}

pub(super) fn revision() -> u64 {
    REVISION.load(Ordering::Acquire)
}

pub(super) fn invalidation_key(key: &str) -> bool {
    matches!(
        key,
        "library"
            | "episode_progress"
            | "settings"
            | "viewing_activity:v1"
            | "continue_hidden"
            | "new_episodes_hidden:v1"
            | "watch_history_hidden:v1"
    ) || key.starts_with("episodes:")
        || key.starts_with("meta_header:")
}

#[derive(Clone, Default)]
pub(super) struct State(Arc<Mutex<Pending>>);

#[derive(Default)]
struct Pending {
    generation: u64,
    view: View,
    running: bool,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct View {
    pub filter: String,
    pub query: String,
    pub sort: i32,
    pub smart: Option<nova_config::SmartCollection>,
}

impl View {
    fn capture(bridge: &Bridge) -> Option<Self> {
        let app = bridge.app()?;
        Some(Self {
            filter: app.get_library_filter_category().to_string(),
            query: app.get_library_query().to_string(),
            sort: app.get_library_sort(),
            smart: bridge.selected_smart_collection(),
        })
    }
}

impl State {
    fn request(&self, view: View) -> bool {
        let mut pending = self.0.lock().unwrap();
        pending.generation = pending.generation.wrapping_add(1);
        pending.view = view;
        if pending.running {
            false
        } else {
            pending.running = true;
            true
        }
    }

    fn job(&self) -> (u64, View) {
        let pending = self.0.lock().unwrap();
        (pending.generation, pending.view.clone())
    }

    fn is_current(&self, generation: u64, version: u64) -> bool {
        revision() == version && self.0.lock().unwrap().generation == generation
    }

    fn finish(&self) {
        self.0.lock().unwrap().running = false;
    }
}

type MetadataLoader<'a> = dyn FnMut(&str, &str) -> (Option<Vec<Video>>, Option<MetaHeader>) + 'a;
type MetadataLru = VecDeque<((String, String), Arc<TitleMetadata>, usize)>;

/// Worker-local LRU. A large library never keeps every parsed response alive
/// at once; active guards pin only the title currently being projected.
pub(super) struct Metadata<'a> {
    cache: std::cell::RefCell<MetadataLru>,
    load: std::cell::RefCell<Box<MetadataLoader<'a>>>,
    current: Box<dyn Fn() -> bool + 'a>,
}

#[derive(Default)]
pub(super) struct TitleMetadata {
    pub episodes: Option<Arc<Vec<Video>>>,
    pub header: Option<Arc<MetaHeader>>,
}

impl<'a> Metadata<'a> {
    fn new(
        load: impl FnMut(&str, &str) -> (Option<Vec<Video>>, Option<MetaHeader>) + 'a,
        current: impl Fn() -> bool + 'a,
    ) -> Self {
        Self {
            cache: Default::default(),
            load: std::cell::RefCell::new(Box::new(load)),
            current: Box::new(current),
        }
    }

    pub(super) fn title(&self, type_: &str, id: &str) -> Arc<TitleMetadata> {
        if !(self.current)() {
            return Arc::default();
        }
        {
            let mut cache = self.cache.borrow_mut();
            if let Some(index) = cache
                .iter()
                .position(|(key, _, _)| key.0 == type_ && key.1 == id)
            {
                let cached = cache.remove(index).unwrap();
                let result = cached.1.clone();
                cache.push_back(cached);
                return result;
            }
        }
        let (episodes, header) = (self.load.borrow_mut())(type_, id);
        // Invalidation can happen while the injected or real cache reader is
        // blocked. Discard that response before it enters even this local LRU.
        if !(self.current)() {
            return Arc::default();
        }
        let title = Arc::new(TitleMetadata {
            episodes: episodes.map(Arc::new),
            header: header.map(Arc::new),
        });
        // Counting serialized bytes needs no second response-sized allocation.
        // Both title count and episode count also bound object overhead.
        struct Size(usize);
        impl std::io::Write for Size {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self.0.saturating_add(bytes.len());
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        const MAX_TITLES: usize = 4;
        const MAX_EPISODES: usize = 2000;
        const MAX_BYTES: usize = 4 * 1024 * 1024;
        let mut size = Size(0);
        let _ = serde_json::to_writer(
            &mut size,
            &(title.episodes.as_deref(), title.header.as_deref()),
        );
        let count = title.episodes.as_ref().map_or(0, |episodes| episodes.len());
        if count <= MAX_EPISODES && size.0 <= MAX_BYTES {
            let mut cache = self.cache.borrow_mut();
            while cache.len() >= MAX_TITLES
                || cache
                    .iter()
                    .map(|(_, title, _)| {
                        title.episodes.as_ref().map_or(0, |episodes| episodes.len())
                    })
                    .sum::<usize>()
                    + count
                    > MAX_EPISODES
                || cache.iter().map(|(_, _, bytes)| bytes).sum::<usize>() + size.0 > MAX_BYTES
            {
                cache.pop_front();
            }
            cache.push_back(((type_.into(), id.into()), title.clone(), size.0));
        }
        title
    }

    pub(super) fn episodes(&self, type_: &str, id: &str) -> Arc<Vec<Video>> {
        self.title(type_, id).episodes.clone().unwrap_or_default()
    }

    pub(super) fn episode_data(&self, type_: &str, id: &str) -> Option<Arc<Vec<Video>>> {
        self.title(type_, id).episodes.clone()
    }
}

struct Prepared {
    cards: Vec<library::LibraryCardSnapshot>,
    home: home::PreparedProgressHome,
    continuing: Vec<ContinueEntry>,
    new: Vec<ContinueEntry>,
    upcoming: Vec<UpcomingEntry>,
    history: Vec<watch_history::HistoryEntry>,
    archive: Vec<watch_history::HistoryEntry>,
    activity_actions: HashMap<(String, String), i32>,
    settings: CacheSettings,
}

fn prepare(
    mut data: Shared,
    view: &View,
    metadata: &Metadata<'_>,
    activity: &HashMap<String, ViewingActivity>,
    new_hidden: &HashMap<String, Vec<String>>,
    hidden: &HashMap<String, u64>,
) -> Prepared {
    let cards = library::prepare_progress_cards(&data, view, metadata, activity);
    let home = home::prepare_progress_home(&mut data, metadata, activity, new_hidden);
    watch_history::prepare_progress_history(&mut data, metadata, hidden);
    // This map covers filtered-out Library titles as well as visible cards.
    let activity_actions = data
        .entries
        .iter()
        .map(|entry| {
            (
                (entry.type_.clone(), entry.id.clone()),
                viewing_menu_action(
                    entry,
                    metadata.episodes(&entry.type_, &entry.id).as_slice(),
                    &data.progress,
                    activity.get(&entry.id),
                ),
            )
        })
        .collect();
    Prepared {
        cards,
        home,
        continuing: std::mem::take(&mut data.continue_list),
        new: std::mem::take(&mut data.new_episodes_list),
        upcoming: std::mem::take(&mut data.upcoming_list),
        history: std::mem::take(&mut data.home_history_list),
        archive: std::mem::take(&mut data.home_history_archive),
        activity_actions,
        settings: std::mem::take(&mut data.cache_settings),
    }
}

/// Progress keys are copied once; record/history clones are bounded to short
/// lock holds. In particular, this avoids repeated `skip(offset)` scans of a
/// HashMap while the UI is trying to finish another watched gesture.
fn snapshot(bridge: &Bridge, current: impl Fn() -> bool) -> Option<Shared> {
    let (mut snapshot, keys) = {
        let state = bridge.shared.lock().ok()?;
        let entries = state
            .entries
            .iter()
            .map(|e| LibraryEntry {
                id: e.id.clone(),
                type_: e.type_.clone(),
                name: e.name.clone(),
                year: e.year.clone(),
                poster_url: e.poster_url.clone(),
                background_url: String::new(),
                genres: e.genres.clone(),
                description: String::new(),
                categories: e.categories.clone(),
                watch_status: e.watch_status,
                imported_tracking_status: e.imported_tracking_status,
                added_at_secs: e.added_at_secs,
            })
            .collect();
        (
            Shared {
                entries,
                cache_settings: state.cache_settings.clone(),
                continue_hidden: state.continue_hidden.clone(),
                home_showcase_displayed: state.home_showcase_displayed.clone(),
                ..Shared::default()
            },
            state.progress.keys().cloned().collect::<Vec<_>>(),
        )
    };
    for keys in keys.chunks(128) {
        if !current() {
            return None;
        }
        let records = {
            let state = bridge.shared.lock().ok()?;
            keys.iter()
                .filter_map(|key| state.progress.get(key).map(|p| (key.clone(), p.clone())))
                .collect::<Vec<_>>()
        };
        snapshot.progress.extend(records);
    }
    current().then_some(snapshot)
}

impl Bridge {
    pub(super) fn request_progress_projection(&self) {
        let Some(view) = View::capture(self) else {
            return;
        };
        let owner = {
            let state = self.shared.lock().unwrap();
            if state.restoring_models {
                return;
            }
            state.progress_projection.clone()
        };
        if !owner.request(view) {
            return;
        }
        let bridge = self.clone();
        let context = profiles::capture();
        let on_failure = owner.clone();
        if let Err(error) = thread::Builder::new()
            .name("nova-progress-view".into())
            .spawn(move || {
                loop {
                    if !profiles::is_current(&context) {
                        owner.finish();
                        return;
                    }
                    let (generation, view) = owner.job();
                    // Activity reconciliation belongs to the ordered writer, not
                    // to an unguarded full-map write by this derived-data worker.
                    // On failure retain the visible in-memory watch state anyway.
                    persistence::flush_report();
                    let version = revision();
                    let current =
                        || profiles::is_current(&context) && owner.is_current(generation, version);
                    let Some(data) = snapshot(&bridge, current) else {
                        if !profiles::is_current(&context) {
                            owner.finish();
                            return;
                        }
                        continue;
                    };
                    let metadata = Metadata::new(
                        |type_, id| {
                            (
                                if type_ == "movie" {
                                    None
                                } else {
                                    read_episodes_cache_for(type_, id)
                                },
                                read_meta_header_for(type_, id),
                            )
                        },
                        current,
                    );
                    let activity = read_viewing_activity();
                    let new_hidden =
                        read_json::<HashMap<String, Vec<String>>>(NEW_EPISODES_HIDDEN_KEY)
                            .unwrap_or_default();
                    let hidden = read_json::<HashMap<String, u64>>(watch_history::HIDDEN_KEY)
                        .unwrap_or_default();
                    let prepared = prepare(data, &view, &metadata, &activity, &new_hidden, &hidden);
                    if !current() {
                        continue;
                    }
                    // Drop the full history and metadata allocations on this worker;
                    // only bounded display lists and visible Library cards travel.
                    drop(metadata);
                    let callback_bridge = bridge.clone();
                    let callback_owner = owner.clone();
                    let callback_context = context.clone();
                    let queued = profiles::invoke_account(move || {
                        callback_owner.finish();
                        if !profiles::is_current(&callback_context) {
                            return;
                        }
                        profiles::with_context(callback_context, || {
                            if !callback_owner.is_current(generation, version)
                                || View::capture(&callback_bridge).as_ref() != Some(&view)
                                || callback_bridge.shared.lock().unwrap().cache_settings
                                    != prepared.settings
                            {
                                callback_bridge.request_progress_projection();
                                return;
                            }
                            callback_bridge.publish_progress_projection(prepared);
                        });
                    });
                    if queued.is_err() {
                        owner.finish();
                    }
                    return;
                }
            })
        {
            on_failure.finish();
            storage::report(storage::Error::new(storage::ErrorKind::Transaction, error));
        }
    }
    fn publish_progress_projection(&self, prepared: Prepared) {
        #[cfg(test)]
        let publish_started = std::time::Instant::now();
        let action = {
            let mut state = self.shared.lock().unwrap();
            if state.restoring_models {
                return;
            }
            state.continue_list = prepared.continuing;
            state.new_episodes_list = prepared.new;
            state.upcoming_list = prepared.upcoming;
            state.home_history_list = prepared.history;
            state.home_history_archive = prepared.archive;
            state
                .modal_item
                .as_ref()
                .and_then(|item| {
                    prepared
                        .activity_actions
                        .get(&(item.type_.clone(), item.id.clone()))
                        .copied()
                })
                .unwrap_or_default()
        };
        if let Some(app) = self.app() {
            app.set_detail_library_activity_action(action);
        }
        self.publish_progress_cards(prepared.cards);
        self.publish_progress_home(prepared.home);
        #[cfg(test)]
        {
            PUBLICATION_MICROS.store(
                publish_started.elapsed().as_micros() as u64,
                Ordering::Release,
            );
            PUBLICATIONS.fetch_add(1, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn entry(id: &str, type_: &str) -> LibraryEntry {
        serde_json::from_value(serde_json::json!({ "id": id, "type_": type_, "name": id })).unwrap()
    }

    #[test]
    fn metadata_lru_bounds_titles_episodes_bytes_and_drops_oversized_responses() {
        let metadata = Metadata::new(
            |_, id| {
                let count = match id {
                    "oversized" => 2001,
                    "byte-heavy" => 2,
                    _ => 600,
                };
                (
                    Some(
                        (0..count)
                            .map(|number| Video {
                                id: number.to_string(),
                                overview: Some("synopsis".repeat(if id == "byte-heavy" {
                                    300_000
                                } else {
                                    100
                                })),
                                ..Default::default()
                            })
                            .collect(),
                    ),
                    None,
                )
            },
            || true,
        );
        for number in 0..12 {
            assert_eq!(metadata.episodes("series", &number.to_string()).len(), 600);
            let cache = metadata.cache.borrow();
            assert!(cache.len() <= 4);
            assert!(
                cache
                    .iter()
                    .map(|(_, title, _)| title.episodes.as_ref().map_or(0, |v| v.len()))
                    .sum::<usize>()
                    <= 2000
            );
            assert!(cache.iter().map(|(_, _, bytes)| bytes).sum::<usize>() <= 4 * 1024 * 1024);
        }
        let large = metadata.episodes("series", "oversized");
        assert_eq!(
            large.len(),
            2001,
            "large responses remain usable one title at a time"
        );
        let weak = Arc::downgrade(&large);
        assert!(
            !metadata
                .cache
                .borrow()
                .iter()
                .any(|(key, _, _)| key.1 == "oversized")
        );
        drop(large);
        assert!(
            weak.upgrade().is_none(),
            "the LRU must not retain oversized response memory"
        );
        let heavy = metadata.episodes("series", "byte-heavy");
        assert_eq!(heavy.len(), 2);
        assert!(
            !metadata
                .cache
                .borrow()
                .iter()
                .any(|(key, _, _)| key.1 == "byte-heavy"),
            "a small episode count cannot bypass the retained byte budget"
        );
    }

    #[test]
    fn prepared_rows_keep_returning_resume_upcoming_and_history_distinct() {
        let mut state = Shared {
            entries: vec![
                entry("resume", "series"),
                entry("returning", "series"),
                entry("caught-up", "series"),
                entry("done", "movie"),
            ],
            ..Default::default()
        };
        for (id, watched, position) in [
            ("resume", false, 100.0),
            ("returning", true, 1000.0),
            ("caught-up", true, 1000.0),
            ("done", true, 1000.0),
        ] {
            let episode_id = if id == "done" { id } else { "e1" };
            state.progress.insert(
                progress_map_key(id, episode_id),
                EpisodeProgress {
                    series_id: id.into(),
                    episode_id: episode_id.into(),
                    watched,
                    position_secs: position,
                    duration_secs: 1000.0,
                    updated_at_secs: 10,
                    play_count: 1,
                    history: Some(PlaybackHistory {
                        type_: if id == "done" { "movie" } else { "series" }.into(),
                        title: id.into(),
                        played_at_secs: 10,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            );
        }
        let loads = std::cell::RefCell::new(HashMap::new());
        let metadata = Metadata::new(
            |type_, id| {
                *loads
                    .borrow_mut()
                    .entry((type_.to_owned(), id.to_owned()))
                    .or_insert(0) += 1;
                let videos = (type_ != "movie").then(|| {
                    (1..=2)
                        .map(|number| Video {
                            id: format!("e{number}"),
                            season: Some(1),
                            episode: Some(number),
                            released: Some(
                                if id == "caught-up" && number == 2 {
                                    "2999-01-01"
                                } else {
                                    "2000-01-01"
                                }
                                .into(),
                            ),
                            ..Default::default()
                        })
                        .collect()
                });
                (videos, None)
            },
            || true,
        );
        let activity = HashMap::from([(
            "returning".into(),
            ViewingActivity {
                phase: ViewingPhase::AwaitingReturn,
            },
        )]);
        let _home = home::prepare_progress_home(&mut state, &metadata, &activity, &HashMap::new());
        assert_eq!(state.continue_list.len(), 1);
        assert_eq!(state.continue_list[0].series_id, "resume");
        assert_eq!(state.continue_list[0].episode_id, "e1");
        assert_eq!(state.new_episodes_list.len(), 1);
        assert_eq!(state.new_episodes_list[0].series_id, "returning");
        assert_eq!(state.new_episodes_list[0].episode_id, "e2");
        assert_eq!(state.upcoming_list.len(), 1);
        assert_eq!(state.upcoming_list[0].series_id, "caught-up");
        let cards = library::prepare_progress_cards(
            &state,
            &View {
                filter: NEW_EPISODES_FILTER.into(),
                ..Default::default()
            },
            &metadata,
            &activity,
        );
        assert_eq!(
            cards.len(),
            1,
            "automatic Library filters use the same prepared facts"
        );
        assert!(
            loads.borrow().values().all(|count| *count == 1),
            "one typed metadata load even with playback history"
        );
        watch_history::prepare_progress_history(&mut state, &metadata, &HashMap::new());
        assert!(
            state.home_history_list.is_empty(),
            "Continue owns the one actual resume"
        );
        assert_eq!(
            state.home_history_archive.len(),
            4,
            "manual display buckets do not discard actual playback history"
        );
    }

    #[test]
    fn blocked_metadata_rejects_real_writes_and_coalesces_to_latest_view() {
        const ROOT: &str = "NOVA_PROGRESS_PROJECTION_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-progress-projection-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root).env("XDG_CACHE_HOME", root.join("cache"))
                .args(["--exact", "app::progress_projection::tests::blocked_metadata_rejects_real_writes_and_coalesces_to_latest_view", "--nocapture"]).output().unwrap();
            let _ = std::fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        storage::init_at(&PathBuf::from(root));
        let writer = persistence::start().unwrap();
        for mutation in 0..5 {
            persistence::flush().unwrap();
            let owner = State::default();
            assert!(owner.request(View::default()));
            let (generation, _) = owner.job();
            let version = revision();
            let (entered, waiting) = mpsc::channel();
            let (release, blocked) = mpsc::channel();
            let held = owner.clone();
            let worker = thread::spawn(move || {
                let metadata = Metadata::new(
                    |_, _| {
                        entered.send(()).unwrap();
                        blocked.recv_timeout(Duration::from_secs(10)).unwrap();
                        (
                            Some(vec![Video {
                                id: "old-response".into(),
                                ..Default::default()
                            }]),
                            None,
                        )
                    },
                    || held.is_current(generation, version),
                );
                metadata.episodes("series", "blocked").is_empty()
                    && !held.is_current(generation, version)
            });
            waiting.recv_timeout(Duration::from_secs(10)).unwrap();
            match mutation {
                0 => write_json(CONTINUE_HIDDEN_KEY, &HashMap::from([("blocked", 10u64)])),
                1 => write_progress_map(&HashMap::from([(
                    progress_map_key("blocked", "e1"),
                    EpisodeProgress {
                        series_id: "blocked".into(),
                        episode_id: "e1".into(),
                        watched: true,
                        ..Default::default()
                    },
                )])),
                2 => {
                    write_episodes_cache_for(
                        "series",
                        "blocked",
                        &[Video {
                            id: "e1".into(),
                            ..Default::default()
                        }],
                    );
                }
                3 => write_json("settings", &CacheSettings::default()),
                4 => write_persisted_library(&[entry("replacement", "series")]),
                _ => unreachable!(),
            }
            assert!(
                revision() != version,
                "real mutation {mutation} must invalidate a cache read before it completes"
            );
            for index in 0..60 {
                assert!(
                    !owner.request(View {
                        query: index.to_string(),
                        ..Default::default()
                    }),
                    "rapid actions reuse the running worker"
                );
            }
            release.send(()).unwrap();
            assert!(
                worker.join().unwrap(),
                "the blocked old read must never reach publication"
            );
            assert_eq!(
                owner.job().1.query,
                "59",
                "only the newest view remains pending"
            );
            owner.finish();
            assert!(
                owner.request(owner.job().1),
                "a delivered/canceled job releases its worker slot"
            );
        }
        drop(writer);
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn prepared_publication_matches_legacy_watch_unwatch_and_smart_filters() {
        const ROOT: &str = "NOVA_PROGRESS_PUBLICATION_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-progress-publication-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root).env("XDG_CACHE_HOME", root.join("cache"))
                .args(["--exact", "app::progress_projection::tests::prepared_publication_matches_legacy_watch_unwatch_and_smart_filters", "--nocapture"]).output().unwrap();
            let _ = std::fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
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
        let writer = persistence::start().unwrap();
        let entries = vec![
            entry("resume", "series"),
            entry("returning", "series"),
            entry("typed", "series"),
            // Persisted Library rows are keyed by native id. Keep this movie
            // distinct while exercising typed joins through series caches.
            entry("typed-movie", "movie"),
        ];
        for e in entries.iter().filter(|e| e.type_ != "movie") {
            let videos = (1..=3)
                .map(|number| Video {
                    id: format!("{}:{number}", e.id),
                    name: format!("Episode {number}"),
                    season: Some(1),
                    episode: Some(number),
                    released: Some(
                        if number == 3 {
                            "2999-01-01"
                        } else {
                            "2000-01-01"
                        }
                        .into(),
                    ),
                    ..Default::default()
                })
                .collect::<Vec<_>>();
            write_episodes_cache_for(&e.type_, &e.id, &videos);
        }
        let smart = nova_config::SmartCollection {
            id: "watched".into(),
            name: "Watched".into(),
            watch_state: nova_config::SmartWatchState::Watched,
            ..Default::default()
        };
        {
            let mut state = bridge.shared.lock().unwrap();
            state.entries = entries.clone();
            state.modal_item = Some(ModalItem {
                details: Default::default(),
                open_token: Arc::new(()),
                pending_watch_now: None,
                episodes_loading: false,
                id: "typed".into(),
                type_: "series".into(),
                request_id: "typed".into(),
                videos: read_episodes_cache_for("series", "typed").unwrap(),
                season_backdrops: HashMap::new(),
                seasons: vec![1],
                season_index: 0,
                episode_page: 0,
                name: "typed".into(),
                year: String::new(),
                poster_url: String::new(),
                background_url: String::new(),
                logo_url: String::new(),
                description: String::new(),
                genres: Vec::new(),
            });
            state.cache_settings.smart_collections = vec![smart.clone()];
            for e in &entries {
                let episode = if e.type_ == "movie" {
                    e.id.clone()
                } else {
                    format!("{}:1", e.id)
                };
                state.progress.insert(
                    progress_map_key(&e.id, &episode),
                    EpisodeProgress {
                        series_id: e.id.clone(),
                        episode_id: episode,
                        watched: e.id != "resume",
                        position_secs: if e.id == "resume" { 100.0 } else { 1000.0 },
                        duration_secs: 1000.0,
                        updated_at_secs: 10,
                        play_count: 1,
                        history: Some(PlaybackHistory {
                            type_: e.type_.clone(),
                            title: e.name.clone(),
                            played_at_secs: 10,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                );
                if e.id == "typed" && e.type_ == "series" {
                    state.progress.insert(
                        progress_map_key("typed", "typed:2"),
                        EpisodeProgress {
                            series_id: "typed".into(),
                            episode_id: "typed:2".into(),
                            watched: true,
                            updated_at_secs: 10,
                            ..Default::default()
                        },
                    );
                }
            }
        }
        write_persisted_library(&entries);
        write_json(
            VIEWING_ACTIVITY_KEY,
            &HashMap::from([(
                "returning",
                ViewingActivity {
                    phase: ViewingPhase::AwaitingReturn,
                },
            )]),
        );
        bridge.refresh_smart_collections();
        let view = app.global::<nova_ui_shared::SmartCollectionsBridge>();
        for watched in [true, false, true] {
            {
                let mut state = bridge.shared.lock().unwrap();
                let p = state
                    .progress
                    .get_mut(&progress_map_key("typed", "typed:2"))
                    .unwrap();
                p.watched = watched;
                p.unwatched_at_secs = if watched { 0 } else { 20 };
                p.updated_at_secs += 1;
            }
            write_progress_map(&bridge.shared.lock().unwrap().progress);
            persistence::flush().unwrap();
            for (filter, selected) in [("", ""), ("Completed", ""), ("", "watched")] {
                app.set_library_filter_category(filter.into());
                view.set_selected_id(selected.into());
                bridge.apply_library_to_ui();
                bridge.rebuild_continue_list();
                bridge.rebuild_upcoming_list();
                bridge.apply_home_to_ui();
                bridge.refresh_detail_library_watched();
                let expected_detail_action = app.get_detail_library_activity_action();
                assert_eq!(expected_detail_action, if watched { 0 } else { 3 });
                let library_facts = || {
                    app.get_library()
                        .iter()
                        .map(|c| {
                            (
                                c.id.to_string(),
                                c.title.to_string(),
                                c.badge.to_string(),
                                c.status_key.to_string(),
                                c.watched_count,
                                c.episode_count,
                                c.watched,
                                c.activity_action,
                            )
                        })
                        .collect::<Vec<_>>()
                };
                let home_facts = || {
                    (
                        app.get_home_continue()
                            .iter()
                            .map(|r| {
                                (
                                    r.id.to_string(),
                                    r.subtitle.to_string(),
                                    r.remaining.to_string(),
                                    r.progress,
                                    r.badge,
                                )
                            })
                            .collect::<Vec<_>>(),
                        app.get_home_new_episodes()
                            .iter()
                            .map(|r| {
                                (
                                    r.id.to_string(),
                                    r.subtitle.to_string(),
                                    r.remaining.to_string(),
                                    r.badge,
                                )
                            })
                            .collect::<Vec<_>>(),
                        app.get_home_upcoming()
                            .iter()
                            .map(|r| {
                                (
                                    r.id.to_string(),
                                    r.subtitle.to_string(),
                                    r.date.to_string(),
                                    r.index,
                                )
                            })
                            .collect::<Vec<_>>(),
                        app.get_home_history_archive_cards()
                            .iter()
                            .map(|r| {
                                (
                                    r.id.to_string(),
                                    r.media_type.to_string(),
                                    r.history_action.to_string(),
                                    r.year.to_string(),
                                    r.progress,
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                };
                let expected_library = library_facts();
                let expected_home = home_facts();
                let data = snapshot(&bridge, || true).unwrap();
                let metadata = Metadata::new(
                    |type_, id| {
                        (
                            if type_ == "movie" {
                                None
                            } else {
                                read_episodes_cache_for(type_, id)
                            },
                            read_meta_header_for(type_, id),
                        )
                    },
                    || true,
                );
                let activity = read_viewing_activity();
                let prepared = prepare(
                    data,
                    &View::capture(&bridge).unwrap(),
                    &metadata,
                    &activity,
                    &HashMap::new(),
                    &HashMap::new(),
                );
                let started = std::time::Instant::now();
                bridge.publish_progress_projection(prepared);
                eprintln!(
                    "prepared UI publication watched={watched} filter={filter:?} smart={selected:?}: {:?}",
                    started.elapsed()
                );
                assert_eq!(
                    library_facts(),
                    expected_library,
                    "Library changed between legacy and prepared projection"
                );
                assert_eq!(
                    home_facts(),
                    expected_home,
                    "Home changed between legacy and prepared projection"
                );
                assert_eq!(
                    app.get_detail_library_activity_action(),
                    expected_detail_action,
                    "a filtered-out title still receives its current Detail activity action"
                );
                assert_eq!(
                    bridge.shared.lock().unwrap().upcoming_list.len(),
                    usize::from(watched)
                );
            }
        }
        drop(writer);
    }
}

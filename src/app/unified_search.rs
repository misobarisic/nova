//! Profile-local search documents and exact result actions. Only the text the
//! user types is passed to catalog search; notes and paths stay on this device.
use super::*;

const ACTION: &str = "novaLocalSearchAction";
const LABEL: &str = "novaSearchLabel";
const CONTEXT: &str = "novaSearchContext";
const LIMIT: usize = 150;
pub(super) const DEFAULT_SCOPE: i32 = 1;

#[derive(Clone)]
struct CorpusStamp {
    semantic: u64,
    clock: Arc<CorpusClock>,
    downloads: Option<DownloadCoordinator>,
    download_revision: u64,
    metadata: u64,
    reveals: u64,
    local: u64,
    instance: u64,
}

impl CorpusStamp {
    fn capture(
        local: u64,
        instance: u64,
        clock: Arc<CorpusClock>,
        downloads: DownloadCoordinator,
    ) -> Self {
        Self {
            semantic: clock.revision(),
            clock,
            download_revision: downloads.semantic_revision(),
            downloads: Some(downloads),
            metadata: nova_providers::metadata_revision(),
            reveals: spoiler::reveal_revision(),
            local,
            instance,
        }
    }

    fn current(&self) -> bool {
        self.semantic == self.clock.revision()
            && self
                .downloads
                .as_ref()
                .is_none_or(|downloads| downloads.semantic_revision() == self.download_revision)
            && self.metadata == nova_providers::metadata_revision()
            && self.reveals == spoiler::reveal_revision()
    }
}

impl PartialEq for CorpusStamp {
    fn eq(&self, other: &Self) -> bool {
        self.semantic == other.semantic
            && self.download_revision == other.download_revision
            && self.metadata == other.metadata
            && self.reveals == other.reveals
            && self.local == other.local
            && self.instance == other.instance
    }
}
impl Eq for CorpusStamp {}

#[derive(Clone, PartialEq, Eq)]
struct SearchReceipt {
    context: profiles::Context,
    query: String,
    stamp: CorpusStamp,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchRequestKind {
    Corpus,
    Availability,
}

#[derive(Clone)]
struct SearchRequest {
    kind: SearchRequestKind,
    revision: u64,
    receipt: SearchReceipt,
    context: profiles::Context,
    bridge: Option<Bridge>,
}

#[derive(Default)]
pub(super) struct SearchWork {
    running: bool,
    revision: u64,
    latest: Option<SearchRequest>,
    // Covers running, queued-publication and completed work. Same-query reuse
    // requires the same profile activation and corpus/visibility receipt.
    wanted: Option<SearchReceipt>,
    projection: catalog::SearchProjectionQueue,
}

enum SearchTask {
    Corpus(Box<SearchRequest>),
    Projection(
        Box<(
            catalog::SearchProjectionRequest,
            catalog::SearchProjectionProcessor,
        )>,
    ),
}

impl SearchWork {
    fn take_task(&mut self) -> Option<SearchTask> {
        if let Some(task) = self.projection.take() {
            return Some(SearchTask::Projection(Box::new(task)));
        }
        self.take_next()
            .map(|request| SearchTask::Corpus(Box::new(request)))
    }
    pub(super) fn projection_current(&self, request: &catalog::SearchProjectionRequest) -> bool {
        self.projection.current(request)
            && self.wanted.as_ref().is_none_or(|wanted| {
                request.same_activation(&wanted.context, wanted.stamp.instance)
                    && request.query() == wanted.query
            })
    }
    pub(super) fn projection_query_current(
        &self,
        request: &catalog::SearchProjectionRequest,
    ) -> bool {
        self.projection.query_current(request)
            && self.wanted.as_ref().is_none_or(|wanted| {
                request.same_activation(&wanted.context, wanted.stamp.instance)
                    && request.query() == wanted.query
            })
    }
    pub(super) fn finish_projection(&mut self, processor: catalog::SearchProjectionProcessor) {
        self.projection.put_processor(processor);
    }
    fn submit(&mut self, mut receipt: SearchReceipt) -> Option<(u64, bool)> {
        if self.wanted.as_ref() == Some(&receipt) {
            return None;
        }
        self.revision = self.revision.wrapping_add(1);
        let scan = receipt.query.chars().count() >= 2;
        let context = receipt.context.clone();
        // Cached receipts carry identity, never a session Arc back-reference.
        receipt.context.session = None;
        self.latest = scan.then(|| SearchRequest {
            kind: SearchRequestKind::Corpus,
            revision: self.revision,
            receipt: receipt.clone(),
            context,
            bridge: None,
        });
        self.wanted = Some(receipt);
        let start = scan && !self.running;
        self.running |= start;
        Some((self.revision, start))
    }

    fn submit_probe(
        &mut self,
        context: profiles::Context,
        bridge: Bridge,
        instance: u64,
        query: &str,
    ) -> bool {
        if self.running || self.latest.is_some() {
            return false;
        }
        let Some(receipt) = self.wanted.as_ref().filter(|receipt| {
            receipt.context == context
                && receipt.stamp.instance == instance
                && receipt.query == query
                && receipt.stamp.current()
        }) else {
            return false;
        };
        self.latest = Some(SearchRequest {
            kind: SearchRequestKind::Availability,
            revision: self.revision,
            receipt: receipt.clone(),
            context,
            bridge: Some(bridge),
        });
        self.running = true;
        true
    }

    // Taking the next request and releasing worker ownership share one mutex.
    // A submit can either wake this worker or start its successor, never vanish
    // between an empty-queue check and a later running=false write.
    fn take_next(&mut self) -> Option<SearchRequest> {
        let next = self.latest.take();
        if next.is_none() {
            self.running = false;
        }
        next
    }

    fn current(&self, request: &SearchRequest) -> bool {
        self.revision == request.revision && self.wanted.as_ref() == Some(&request.receipt)
    }

    fn abandon(&mut self, request: &SearchRequest) {
        if self.current(request) {
            self.wanted = None;
        }
    }
}

struct SpoilerSnapshot {
    protected: bool,
    watched: HashSet<String>,
    revealed: HashSet<(String, String, String)>,
}

impl SpoilerSnapshot {
    fn hidden(&self, kind: &str, title: &str, episode: &str) -> bool {
        self.protected
            && kind != "movie"
            && !self.watched.contains(&progress_map_key(title, episode))
            && !self
                .revealed
                .contains(&(kind.into(), title.into(), episode.into()))
    }
}

// Session replacement must not create another scanner while an old inventory
// read is blocked. This one process lane carries the newest owner/Bridge and
// retains at most one active and one pending activation, across all windows.
static LOCAL_SEARCH_WORK: LazyLock<Arc<Mutex<SearchWork>>> = LazyLock::new(Default::default);
static NEXT_STATE_INSTANCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, PartialEq, Eq)]
struct SavedSearchFacts {
    name: String,
    year: String,
    poster: String,
}

#[derive(Clone, PartialEq, Eq)]
struct ProgressTitleFact {
    parent: String,
    kind: String,
    title: String,
    poster: String,
}

#[derive(Default, PartialEq, Eq)]
struct ProgressFacts {
    watched: HashSet<String>,
    titles: HashMap<String, ProgressTitleFact>,
}

impl ProgressFacts {
    fn capture(rows: &HashMap<String, EpisodeProgress>) -> Self {
        Self {
            watched: rows
                .iter()
                .filter(|(_, row)| row.watched)
                .map(|(key, _)| key.clone())
                .collect(),
            titles: rows
                .iter()
                .filter_map(|(key, row)| {
                    let history = row.history.as_ref()?;
                    (!history.type_.is_empty() && !history.title.is_empty()).then(|| {
                        (
                            key.clone(),
                            ProgressTitleFact {
                                parent: row.series_id.clone(),
                                kind: history.type_.clone(),
                                title: history.title.clone(),
                                poster: history.poster_url.clone(),
                            },
                        )
                    })
                })
                .collect(),
        }
    }

    fn matches(&self, rows: &HashMap<String, EpisodeProgress>) -> bool {
        let mut watched = 0;
        let mut titles = 0;
        for (key, row) in rows {
            if row.watched {
                watched += 1;
                if !self.watched.contains(key) {
                    return false;
                }
            }
            if let Some(history) = row
                .history
                .as_ref()
                .filter(|history| !history.type_.is_empty() && !history.title.is_empty())
            {
                titles += 1;
                if !self.titles.get(key).is_some_and(|fact| {
                    fact.parent == row.series_id
                        && fact.kind == history.type_
                        && fact.title == history.title
                        && fact.poster == history.poster_url
                }) {
                    return false;
                }
            }
        }
        watched == self.watched.len() && titles == self.titles.len()
    }
}

#[derive(Clone, Copy)]
enum CorpusDomain {
    Saved,
    Progress,
    Metadata,
    Bookmarks,
    Visibility,
    Downloads,
}

#[derive(Default)]
struct CorpusFacts {
    progress: Option<Arc<ProgressFacts>>,
    visibility: Option<(Language, bool)>,
    snapshots: HashMap<String, String>,
    revisions: [u64; 6],
    admitted_progress: bool,
}

#[derive(Default)]
struct CorpusClock {
    revision: AtomicU64,
    facts: Mutex<CorpusFacts>,
}

impl CorpusClock {
    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    fn advance(&self, domain: CorpusDomain) {
        let mut facts = self.facts.lock().unwrap();
        facts.revisions[domain as usize] += 1;
        self.revision.fetch_add(1, Ordering::AcqRel);
    }

    fn progress(&self, rows: &HashMap<String, EpisodeProgress>, admitted: bool) -> bool {
        let mut facts = self.facts.lock().unwrap();
        let changed = facts
            .progress
            .as_ref()
            .is_some_and(|old| !old.matches(rows));
        if facts.progress.is_none() || changed {
            facts.progress = Some(Arc::new(ProgressFacts::capture(rows)));
        }
        if changed {
            facts.revisions[CorpusDomain::Progress as usize] += 1;
            facts.admitted_progress |= admitted;
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        changed
    }

    fn committed_progress(&self, prepared: &Arc<ProgressFacts>) -> bool {
        let mut facts = self.facts.lock().unwrap();
        let changed = facts.progress.as_ref().is_some_and(|old| old != prepared);
        if facts.progress.is_none() || changed {
            facts.progress = Some(prepared.clone());
        }
        if changed {
            facts.revisions[CorpusDomain::Progress as usize] += 1;
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        // Accepted progress already cancelled the old semantic receipt. Its
        // durable commit must post once even when these facts are unchanged.
        let admitted = std::mem::take(&mut facts.admitted_progress);
        changed || admitted
    }

    fn visibility(&self, settings: &CacheSettings) -> bool {
        self.visibility_facts((settings.language, settings.spoiler_protection))
    }

    fn visibility_facts(&self, value: (Language, bool)) -> bool {
        let mut facts = self.facts.lock().unwrap();
        let changed = facts.visibility.is_some_and(|old| old != value);
        facts.visibility = Some(value);
        if changed {
            facts.revisions[CorpusDomain::Visibility as usize] += 1;
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        changed
    }

    fn snapshot(&self, key: &str, fingerprint: String, domain: CorpusDomain) -> bool {
        let mut facts = self.facts.lock().unwrap();
        if facts.snapshots.get(key) == Some(&fingerprint) {
            return false;
        }
        facts.snapshots.insert(key.into(), fingerprint);
        facts.revisions[domain as usize] += 1;
        self.revision.fetch_add(1, Ordering::AcqRel);
        true
    }

    fn remember(&self, key: &str, fingerprint: String) {
        self.facts
            .lock()
            .unwrap()
            .snapshots
            .insert(key.into(), fingerprint);
    }

    fn seed(&self, key: &str, fingerprint: String) {
        self.facts
            .lock()
            .unwrap()
            .snapshots
            .entry(key.into())
            .or_insert(fingerprint);
    }
}

#[derive(Clone)]
struct RegisteredCorpus {
    owner: profiles::ProfileStore,
    profile: String,
    epoch: u64,
    instance: u64,
    clock: Arc<CorpusClock>,
    bridge: Bridge,
}

#[derive(Default)]
struct CorpusRefresh {
    posted: bool,
    latest: Option<(profiles::Context, Bridge, u64)>,
}

static ACTIVE_CORPUS: Mutex<Option<RegisteredCorpus>> = Mutex::new(None);
static CORPUS_REFRESH: Mutex<CorpusRefresh> = Mutex::new(CorpusRefresh {
    posted: false,
    latest: None,
});

fn post_corpus_refresh(active: &RegisteredCorpus) {
    let context = profiles::active_context();
    if context.id != active.profile || context.epoch != active.epoch {
        return;
    }
    let post = {
        let mut pending = CORPUS_REFRESH.lock().unwrap();
        pending.latest = Some((context, active.bridge.clone(), active.instance));
        let post = !pending.posted;
        pending.posted = true;
        post
    };
    if !post {
        return;
    }
    if profiles::invoke_account(|| {
        let latest = {
            let mut pending = CORPUS_REFRESH.lock().unwrap();
            pending.posted = false;
            pending.latest.take()
        };
        let Some((context, bridge, instance)) = latest else {
            return;
        };
        if !profiles::is_current(&context) {
            return;
        }
        profiles::with_context(context, || {
            if bridge
                .app()
                .is_some_and(|app| app.get_discover_search_open())
            {
                let query = {
                    let state = bridge.shared.lock().unwrap();
                    if state.unified_search.instance != instance {
                        return;
                    }
                    state.unified_search.query.clone()
                };
                bridge.refresh_local_search(&query);
            }
        });
    })
    .is_err()
    {
        let mut pending = CORPUS_REFRESH.lock().unwrap();
        pending.posted = false;
        pending.latest = None;
    }
}

fn active_corpus(
    owner: &profiles::ProfileStore,
    global_metadata: bool,
) -> Option<RegisteredCorpus> {
    ACTIVE_CORPUS
        .lock()
        .unwrap()
        .as_ref()
        .filter(|active| global_metadata || active.owner == *owner)
        .cloned()
}

fn json_fingerprint(raw: &str) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(raw).ok()?;
    value.sort_all_objects();
    Some(nova_providers::catalog_grouping::projection_fingerprint(
        &value,
    ))
}

fn saved_fingerprint(entries: &[LibraryEntry]) -> String {
    let mut facts: Vec<_> = entries
        .iter()
        .map(|entry| {
            (
                &entry.type_,
                &entry.id,
                &entry.name,
                &entry.year,
                &entry.poster_url,
            )
        })
        .collect();
    facts.sort();
    nova_providers::catalog_grouping::projection_fingerprint(&facts)
}

/// Called only after ordered queue admission, not on queue failure. It rejects
/// a paused scan immediately; UI refresh waits for the durable commit hook.
pub(super) fn progress_admitted(
    owner: &profiles::ProfileStore,
    rows: &HashMap<String, EpisodeProgress>,
) {
    if let Some(active) = active_corpus(owner, false) {
        active.clock.progress(rows, true);
    }
}

// Preparation owns only decoded semantic facts, never an activation clock.
// Resolve the current clock at successful commit so A→B→A cannot notify old A.
pub(super) struct PreparedSnapshotChange {
    metadata: bool,
    change: PreparedSnapshotFacts,
}
enum PreparedSnapshotFacts {
    Progress(Arc<ProgressFacts>),
    Visibility((Language, bool)),
    Fingerprint {
        key: String,
        value: String,
        domain: CorpusDomain,
    },
}
pub(super) struct SnapshotRefresh(RegisteredCorpus);

/// Decode before entering the durable owner/write boundary. A failed or
/// superseded write can discard/reuse this value without notifying search.
pub(super) fn prepare_snapshot_change(key: &str, raw: &str) -> Option<PreparedSnapshotChange> {
    let metadata = key.starts_with("episodes:") || key.starts_with("meta_header:");
    let change = if key == EPISODE_PROGRESS_KEY {
        let rows = serde_json::from_str::<HashMap<String, EpisodeProgress>>(raw).ok()?;
        PreparedSnapshotFacts::Progress(Arc::new(ProgressFacts::capture(&rows)))
    } else if key == "settings" {
        let settings = serde_json::from_str::<CacheSettings>(raw).ok()?;
        PreparedSnapshotFacts::Visibility((settings.language, settings.spoiler_protection))
    } else if key == "library" {
        let entries = serde_json::from_str::<Vec<LibraryEntry>>(raw).ok()?;
        PreparedSnapshotFacts::Fingerprint {
            key: key.into(),
            value: saved_fingerprint(&entries),
            domain: CorpusDomain::Saved,
        }
    } else if metadata || matches!(key, bookmarks::BOOKMARKS_KEY | profile_policy::KEY) {
        PreparedSnapshotFacts::Fingerprint {
            key: key.into(),
            value: json_fingerprint(raw)?,
            domain: if metadata {
                CorpusDomain::Metadata
            } else if key == bookmarks::BOOKMARKS_KEY {
                CorpusDomain::Bookmarks
            } else {
                CorpusDomain::Visibility
            },
        }
    } else {
        return None;
    };
    Some(PreparedSnapshotChange { metadata, change })
}

/// Call only after successful durable save, while still inside its owner/write
/// boundary. This updates plain receipt facts only: no Shared, KV, UI or post.
pub(super) fn commit_snapshot_change(
    owner: &profiles::ProfileStore,
    prepared: &PreparedSnapshotChange,
) -> Option<SnapshotRefresh> {
    let active = active_corpus(owner, prepared.metadata)?;
    let changed = match &prepared.change {
        PreparedSnapshotFacts::Progress(facts) => active.clock.committed_progress(facts),
        PreparedSnapshotFacts::Visibility(value) => active.clock.visibility_facts(*value),
        PreparedSnapshotFacts::Fingerprint { key, value, domain } => {
            active.clock.snapshot(key, value.clone(), *domain)
        }
    };
    changed.then_some(SnapshotRefresh(active))
}

/// Post only after releasing durable owner/write/store/queue locks.
pub(super) fn post_snapshot_change(refresh: SnapshotRefresh) {
    post_corpus_refresh(&refresh.0);
}

pub(super) struct State {
    revision: u64,
    corpus_revision: u64,
    clock: Arc<CorpusClock>,
    requested_semantic: u64,
    downloads_seen: u64,
    last_availability_probe: Option<std::time::Instant>,
    instance: u64,
    work: Arc<Mutex<SearchWork>>,
    query: String,
    scope: i32,
    results: Arc<Vec<MetaPreview>>,
    results_stamp: Option<CorpusStamp>,
    library_membership: HashMap<String, HashSet<String>>,
    library_membership_count: usize,
    saved_facts: HashMap<String, HashMap<String, SavedSearchFacts>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            revision: 0,
            corpus_revision: 0,
            clock: Arc::default(),
            requested_semantic: 0,
            downloads_seen: 0,
            last_availability_probe: None,
            instance: NEXT_STATE_INSTANCE.fetch_add(1, Ordering::Relaxed),
            work: LOCAL_SEARCH_WORK.clone(),
            query: String::new(),
            scope: DEFAULT_SCOPE,
            results: Arc::default(),
            results_stamp: None,
            library_membership: HashMap::new(),
            library_membership_count: 0,
            saved_facts: HashMap::new(),
        }
    }
}

impl State {
    fn contains_saved(&self, kind: &str, id: &str) -> bool {
        self.library_membership
            .get(kind)
            .is_some_and(|ids| ids.contains(id))
    }

    fn library_matches(&self, entries: &[LibraryEntry]) -> bool {
        entries.len() == self.library_membership_count
            && entries.iter().all(|entry| {
                self.contains_saved(&entry.type_, &entry.id) && self.saved_facts_match(entry)
            })
    }

    fn saved_facts_match(&self, entry: &LibraryEntry) -> bool {
        self.saved_facts
            .get(&entry.type_)
            .and_then(|titles| titles.get(&entry.id))
            .is_some_and(|facts| {
                facts.name == entry.name
                    && facts.year == entry.year
                    && facts.poster == entry.poster_url
            })
    }

    fn remember_library(&mut self, entries: &[LibraryEntry]) {
        self.clock.remember("library", saved_fingerprint(entries));
        self.library_membership.clear();
        self.saved_facts.clear();
        self.library_membership_count = entries.len();
        for entry in entries {
            self.library_membership
                .entry(entry.type_.clone())
                .or_default()
                .insert(entry.id.clone());
            self.saved_facts
                .entry(entry.type_.clone())
                .or_default()
                .insert(
                    entry.id.clone(),
                    SavedSearchFacts {
                        name: entry.name.clone(),
                        year: entry.year.clone(),
                        poster: entry.poster_url.clone(),
                    },
                );
        }
    }

    pub(super) fn clear_scope(&mut self) {
        self.scope = DEFAULT_SCOPE;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum Action {
    Title,
    Episode { title: String, video: Video },
    Download { job: String, title: String },
    Bookmark { scene: String, title: String },
}

fn action(meta: &MetaPreview) -> Option<Action> {
    serde_json::from_value(meta.extra.get(ACTION)?.clone()).ok()
}

fn title_id(meta: &MetaPreview) -> &str {
    match meta.extra.get("novaSearchTitle").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => &meta.id,
    }
}

pub(super) fn global_id(id: &str) -> bool {
    nova_providers::ExternalId::parse(id).is_some()
        || nova_providers::catalog_grouping::ParentIdentity::parse(id).is_some()
        || id.strip_prefix("kitsu:").is_some_and(|id| {
            !id.is_empty()
                && id.bytes().all(|byte| byte.is_ascii_digit())
                && id.parse::<u64>().is_ok_and(|id| id > 0)
        })
}

/// Opaque IDs have meaning only inside their addon. Matching display names
/// never proves that two titles or editions are interchangeable.
pub(super) fn result_key(meta: &MetaPreview) -> (String, String, String) {
    let source = if global_id(&meta.id) || action(meta).is_some() {
        String::new()
    } else {
        meta.extra
            .get("novaSourceUrl")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .into()
    };
    (meta.type_.clone(), meta.id.clone(), source)
}

#[cfg(test)]
pub(super) fn restored_focus(
    old: &[MetaPreview],
    new: &[MetaPreview],
    index: usize,
) -> Option<usize> {
    let key = result_key(old.get(index)?);
    new.iter().position(|meta| result_key(meta) == key)
}

pub(super) fn tag_catalog(meta: &mut MetaPreview, source: &str) {
    // Addons cannot manufacture a device-local action or its trusted caption.
    for key in [ACTION, LABEL, CONTEXT, "novaSearchTitle"] {
        meta.extra.remove(key);
    }
    meta.extra.insert("novaSourceUrl".into(), source.into());
}

fn document(
    preview: MetaPreview,
    id: String,
    name: String,
    label: &str,
    context: String,
    action: Action,
) -> MetaPreview {
    let mut result = preview;
    result
        .extra
        .insert("novaSearchTitle".into(), result.id.clone().into());
    result.id = id;
    result.name = name;
    result
        .extra
        .insert(ACTION.into(), serde_json::to_value(action).unwrap());
    result.extra.insert(LABEL.into(), label.into());
    result.extra.insert(CONTEXT.into(), context.into());
    result
}

fn library_preview(entry: &LibraryEntry) -> MetaPreview {
    MetaPreview {
        id: entry.id.clone(),
        type_: entry.type_.clone(),
        name: entry.name.clone(),
        poster: (!entry.poster_url.is_empty()).then(|| entry.poster_url.clone()),
        ..Default::default()
    }
}

fn matches(query: &str, title: &str, context: &str) -> bool {
    let haystack = format!("{title} {context}").to_lowercase();
    query
        .split_whitespace()
        .all(|term| haystack.contains(&term.to_lowercase()))
}

fn episode_result_id(kind: &str, title: &str, episode: &str) -> String {
    format!(
        "local:episode:{}",
        serde_json::to_string(&(kind, title, episode)).unwrap()
    )
}

fn document_order(a: &MetaPreview, b: &MetaPreview) -> std::cmp::Ordering {
    a.name
        .to_lowercase()
        .cmp(&b.name.to_lowercase())
        .then_with(|| a.id.cmp(&b.id))
        .then_with(|| a.type_.cmp(&b.type_))
}

fn retain_result(results: &mut Vec<MetaPreview>, hit: MetaPreview) {
    let index = results
        .binary_search_by(|existing| document_order(existing, &hit))
        .unwrap_or_else(|index| index);
    if index < LIMIT {
        results.insert(index, hit);
        results.truncate(LIMIT);
    }
}

/// Each scope retains its own best matches before the presentation cap. A long
/// episode inventory must not evict Library, Download or Bookmark documents.
/// The union is bounded to five budgets, irrespective of cache size.
#[derive(Default)]
struct ScopedResults {
    buckets: [Vec<MetaPreview>; 5],
}

impl ScopedResults {
    fn insert(&mut self, hit: MetaPreview, saved: bool) {
        let kind = action(&hit);
        let scopes = [
            true,
            matches!(kind, Some(Action::Title))
                || (saved && matches!(kind, Some(Action::Download { .. }))),
            matches!(kind, Some(Action::Episode { .. })),
            matches!(kind, Some(Action::Download { .. })),
            matches!(kind, Some(Action::Bookmark { .. })),
        ];
        for (bucket, included) in self.buckets.iter_mut().zip(scopes) {
            if included {
                // A usable movie download upgrades the saved title's action.
                bucket.retain(|existing| existing.type_ != hit.type_ || existing.id != hit.id);
                retain_result(bucket, hit.clone());
            }
        }
    }

    fn finish(self) -> Vec<MetaPreview> {
        let mut seen = HashSet::new();
        let mut results: Vec<_> = self
            .buckets
            .into_iter()
            .flatten()
            .filter(|meta| seen.insert((meta.type_.clone(), meta.id.clone())))
            .collect();
        results.sort_by(document_order);
        results
    }
}

#[cfg(test)]
fn saved_documents<'a>(
    query: &'a str,
    entries: &'a [LibraryEntry],
) -> impl Iterator<Item = MetaPreview> + 'a {
    saved_documents_current(query, entries, || true)
}

fn saved_documents_current<'a>(
    query: &'a str,
    entries: &'a [LibraryEntry],
    current: impl Fn() -> bool + 'a,
) -> impl Iterator<Item = MetaPreview> + 'a {
    let owner = profiles::ProfileStore::capture();
    entries
        .iter()
        .take_while(move |_| current())
        .filter(move |entry| {
            matches(query, &entry.name, &entry.year)
                && profile_policy::allows_browse_title(&owner, &entry.type_, &entry.id)
        })
        .map(|entry| {
            document(
                library_preview(entry),
                entry.id.clone(),
                entry.name.clone(),
                "Saved",
                entry.year.clone(),
                Action::Title,
            )
        })
}

#[cfg(test)]
fn saved_results(query: &str, entries: &[LibraryEntry]) -> Vec<MetaPreview> {
    let mut results = Vec::new();
    for hit in saved_documents(query, entries) {
        retain_result(&mut results, hit);
    }
    results
}

fn usable(job: &DownloadJob, owner: &profiles::ProfileStore) -> bool {
    job.phase == crate::download::DownloadPhase::Completed
        && smart_offline::allows_download(owner, job)
        && profile_policy::allows_browse_title(owner, &job.media_type, &job.media_id)
        && job.artifact_path.as_ref().is_some_and(|path| {
            path.metadata()
                .is_ok_and(|meta| meta.is_file() && meta.len() > 0)
                && std::fs::File::open(path).is_ok()
        })
}

fn find_current_video<'a>(
    videos: &'a [Video],
    id: &str,
    current: &impl Fn() -> bool,
) -> Option<&'a Video> {
    for (index, video) in videos.iter().enumerate() {
        if index % 64 == 0 && !current() {
            return None;
        }
        if video.id == id {
            return Some(video);
        }
    }
    None
}

#[cfg(test)]
fn collect_results(
    query: &str,
    entries: &[LibraryEntry],
    known: &[MetaPreview],
    jobs: Vec<DownloadJob>,
    shared: &profile_session::SessionState,
) -> Vec<MetaPreview> {
    let state = shared.lock().unwrap();
    let spoilers = SpoilerSnapshot {
        protected: state.cache_settings.spoiler_protection,
        watched: state
            .progress
            .iter()
            .filter(|(_, progress)| progress.watched)
            .map(|(key, _)| key.clone())
            .collect(),
        revealed: spoiler::revealed_snapshot(&profiles::capture()),
    };
    drop(state);
    collect_results_current(query, entries, known, jobs, &spoilers, || true, || {}).unwrap()
}

fn collect_results_current(
    query: &str,
    entries: &[LibraryEntry],
    known: &[MetaPreview],
    jobs: Vec<DownloadJob>,
    spoilers: &SpoilerSnapshot,
    current: impl Fn() -> bool,
    foreground: impl Fn(),
) -> Option<Vec<MetaPreview>> {
    collect_results_with_reader_cooperative(
        CollectorInput {
            query,
            entries,
            known,
            jobs,
            spoilers,
        },
        current,
        |kind, id| {
            #[cfg(test)]
            if let Some(videos) = search_test_read_inventory(kind, id) {
                return videos;
            }
            read_episodes_cache_for(kind, id).unwrap_or_default()
        },
        foreground,
    )
}

#[cfg(test)]
fn collect_results_with_reader(
    query: &str,
    entries: &[LibraryEntry],
    known: &[MetaPreview],
    jobs: Vec<DownloadJob>,
    spoilers: &SpoilerSnapshot,
    current: impl Fn() -> bool,
    read_inventory: impl Fn(&str, &str) -> Vec<Video>,
) -> Option<Vec<MetaPreview>> {
    collect_results_with_reader_cooperative(
        CollectorInput {
            query,
            entries,
            known,
            jobs,
            spoilers,
        },
        current,
        read_inventory,
        || {},
    )
}

struct CollectorInput<'a> {
    query: &'a str,
    entries: &'a [LibraryEntry],
    known: &'a [MetaPreview],
    jobs: Vec<DownloadJob>,
    spoilers: &'a SpoilerSnapshot,
}

fn collect_results_with_reader_cooperative(
    input: CollectorInput<'_>,
    current: impl Fn() -> bool,
    read_inventory: impl Fn(&str, &str) -> Vec<Video>,
    foreground: impl Fn(),
) -> Option<Vec<MetaPreview>> {
    let CollectorInput {
        query,
        entries,
        known,
        jobs,
        spoilers,
    } = input;
    if !current() {
        return None;
    }
    let owner = profiles::ProfileStore::capture();
    let mut results = ScopedResults::default();
    for hit in saved_documents_current(query, entries, &current) {
        foreground();
        // Apply movie upgrades before the bounded selection, so changing a
        // caption cannot discard a previously truncated runner-up title.
        if !jobs.iter().any(|job| {
            current()
                && job.media_type == "movie"
                && job.media_type == hit.type_
                && job.media_id == hit.id
                && usable(job, &owner)
                && matches(query, &job.title, "")
        }) {
            results.insert(hit, true);
        }
    }
    if !current() {
        return None;
    }
    let mut saved = HashSet::new();
    for entry in entries {
        foreground();
        if !current() {
            return None;
        }
        saved.insert((entry.type_.as_str(), entry.id.as_str()));
    }
    let mut titles = HashMap::new();
    for meta in known {
        foreground();
        if !current() {
            return None;
        }
        titles.insert((meta.type_.clone(), meta.id.clone()), meta.clone());
    }
    for entry in entries {
        foreground();
        if !current() {
            return None;
        }
        titles.insert(
            (entry.type_.clone(), entry.id.clone()),
            library_preview(entry),
        );
    }
    if !current() {
        return None;
    }
    let bookmarks = bookmarks::load_bookmarks().unwrap_or_default();
    for (key, rows) in &bookmarks {
        if !current() {
            return None;
        }
        let Ok((kind, id, episode)) = serde_json::from_str::<(String, String, String)>(key) else {
            continue;
        };
        if kind.is_empty() || id.is_empty() || episode.is_empty() {
            continue;
        }
        let metadata = rows
            .iter()
            .take_while(|_| current())
            .filter_map(|bookmark| bookmark.metadata.as_ref())
            .find(|meta| {
                meta.media_type == kind
                    && meta.title_id == id
                    && meta.episode_id == episode
                    && !meta.title.is_empty()
            });
        titles
            .entry((kind.clone(), id.clone()))
            .or_insert_with(|| MetaPreview {
                id,
                type_: kind,
                name: metadata.map_or_else(
                    || text::tr("Untitled saved title").into(),
                    |meta| meta.title.clone(),
                ),
                poster: metadata.and_then(|meta| {
                    (!meta.poster_url.is_empty()).then(|| meta.poster_url.clone())
                }),
                ..Default::default()
            });
    }
    for job in &jobs {
        foreground();
        if !current() {
            return None;
        }
        if !smart_offline::allows_download(&owner, job) {
            continue;
        }
        titles
            .entry((job.media_type.clone(), job.media_id.clone()))
            .or_insert_with(|| MetaPreview {
                id: job.media_id.clone(),
                type_: job.media_type.clone(),
                name: job.title.clone(),
                ..Default::default()
            });
    }

    let mut job_groups = HashMap::<_, Vec<_>>::new();
    for job in &jobs {
        foreground();
        if !current() {
            return None;
        }
        job_groups
            .entry((job.media_type.clone(), job.media_id.clone()))
            .or_default()
            .push(job);
    }
    let mut bookmark_groups = HashMap::<_, Vec<_>>::new();
    for (key, rows) in &bookmarks {
        if !current() {
            return None;
        }
        let Ok((kind, id, episode)) = serde_json::from_str::<(String, String, String)>(key) else {
            continue;
        };
        if !kind.is_empty() && !id.is_empty() && !episode.is_empty() {
            bookmark_groups
                .entry((kind, id))
                .or_default()
                .push((key, episode, rows));
        }
    }
    // Metadata caches are device-wide, unlike notes/history. Only index a
    // parent evidenced in this profile. Load one inventory at a time rather
    // than retaining every episode in this profile's known-title corpus.
    for ((kind, id), preview) in &titles {
        foreground();
        if !current() {
            return None;
        }
        if !profile_policy::allows_browse_title(&owner, kind, id) {
            continue;
        }
        if !current() {
            return None;
        }
        let videos = read_inventory(kind, id);
        let active = ACTIVE_CORPUS.lock().unwrap().clone();
        if let Some(active) = active.filter(|active| active.profile == owner.id())
            && let Ok(mut value) = serde_json::to_value(&videos)
        {
            value.sort_all_objects();
            active.clock.seed(
                &episodes_key(kind, id),
                nova_providers::catalog_grouping::projection_fingerprint(&value),
            );
        }
        for (index, video) in videos.iter().enumerate() {
            if index % 64 == 0 {
                foreground();
                if !current() {
                    return None;
                }
            }
            if video.id.is_empty() {
                continue;
            }
            let hidden = spoilers.hidden(kind, id, &video.id);
            let label = if hidden {
                episode_se_label(video)
            } else {
                stream_episode_context_label(video)
            };
            if matches(query, &label, &preview.title()) {
                results.insert(
                    document(
                        preview.clone(),
                        episode_result_id(kind, id, &video.id),
                        label,
                        "Episode",
                        preview.title(),
                        Action::Episode {
                            title: id.clone(),
                            video: video.clone(),
                        },
                    ),
                    false,
                );
            }
        }
        for job in job_groups
            .get(&(kind.clone(), id.clone()))
            .into_iter()
            .flatten()
        {
            if !current() {
                return None;
            }
            if !usable(job, &owner) {
                continue;
            }
            let video = find_current_video(&videos, &job.request_id, &current);
            let hidden = spoilers.hidden(&job.media_type, &job.media_id, &job.request_id);
            let episode = video
                .map(|video| {
                    if hidden {
                        episode_se_label(video)
                    } else {
                        stream_episode_context_label(video)
                    }
                })
                .unwrap_or_else(|| {
                    if job.media_type == "movie" {
                        String::new()
                    } else if hidden {
                        spoiler::numbered_download_label(None, &job.request_id)
                    } else {
                        job.request_id.clone()
                    }
                });
            if !matches(query, &job.title, &episode) {
                continue;
            }
            let name = if episode.is_empty() {
                job.title.clone()
            } else {
                format!("{} · {episode}", job.title)
            };
            let id = if job.media_type == "movie" {
                job.media_id.clone()
            } else {
                format!("local:download:{}", job.id)
            };
            let hit = document(
                preview.clone(),
                id,
                name,
                "Available offline · Play",
                episode,
                Action::Download {
                    job: job.id.clone(),
                    title: job.media_id.clone(),
                },
            );
            results.insert(
                hit,
                saved.contains(&(job.media_type.as_str(), job.media_id.as_str())),
            );
        }
        for (key, episode_id, rows) in bookmark_groups
            .get(&(kind.clone(), id.clone()))
            .into_iter()
            .flatten()
        {
            if !current() {
                return None;
            }
            let hidden = spoilers.hidden(kind, id, episode_id);
            let episode = find_current_video(&videos, episode_id, &current)
                .map(|video| {
                    if hidden {
                        episode_se_label(video)
                    } else {
                        stream_episode_context_label(video)
                    }
                })
                .unwrap_or_else(|| {
                    if kind == "movie" {
                        String::new()
                    } else if hidden {
                        spoiler::numbered_download_label(None, episode_id)
                    } else {
                        episode_id.clone()
                    }
                });
            for (index, bookmark) in rows.iter().enumerate() {
                if index % 64 == 0 && !current() {
                    return None;
                }
                if !bookmark.position.is_finite() || bookmark.position < 0.0 {
                    continue;
                }
                let context = format!(
                    "{} · {episode} · {}",
                    preview.title(),
                    text::journal_duration(bookmark.position)
                );
                if !matches(query, &bookmark.note, &context) {
                    continue;
                }
                let scene = bookmarks::scene_id(key, bookmark);
                results.insert(
                    document(
                        preview.clone(),
                        format!("local:bookmark:{scene}"),
                        if bookmark.note.is_empty() {
                            preview.title()
                        } else {
                            bookmark.note.clone()
                        },
                        "Bookmark",
                        context,
                        Action::Bookmark {
                            scene,
                            title: id.clone(),
                        },
                    ),
                    false,
                );
            }
        }
    }
    // Never collapse unproven equal-looking display names.
    current().then(|| results.finish())
}

#[derive(Clone)]
pub(super) struct ProjectionLocal {
    pub(super) scope: i32,
    pub(super) results: Arc<Vec<MetaPreview>>,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct PresentationStamp {
    corpus: CorpusStamp,
    metadata_domain: u64,
    visibility: u64,
    progress: u64,
}
impl PresentationStamp {
    pub(super) fn current(&self) -> bool {
        self.corpus.current()
    }
    pub(super) fn instance(&self) -> u64 {
        self.corpus.instance
    }
    pub(super) fn metadata_key(&self) -> (u64, u64) {
        (self.corpus.metadata, self.metadata_domain)
    }
    pub(super) fn visibility_key(&self) -> (u64, u64, u64) {
        (self.visibility, self.progress, self.corpus.reveals)
    }
}

impl State {
    pub(super) fn projection_local(&self) -> ProjectionLocal {
        ProjectionLocal {
            scope: self.scope,
            results: if self.results_stamp.as_ref().is_none_or(CorpusStamp::current) {
                self.results.clone()
            } else {
                Arc::default()
            },
        }
    }
    pub(super) fn presentation_stamp(&self, downloads: DownloadCoordinator) -> PresentationStamp {
        let facts = self.clock.facts.lock().unwrap();
        PresentationStamp {
            corpus: CorpusStamp::capture(
                self.corpus_revision,
                self.instance,
                self.clock.clone(),
                downloads,
            ),
            metadata_domain: facts.revisions[CorpusDomain::Metadata as usize],
            visibility: facts.revisions[CorpusDomain::Visibility as usize],
            progress: facts.revisions[CorpusDomain::Progress as usize],
        }
    }
}

pub(super) fn memory_saved_document(
    query: &str,
    kind: &str,
    id: &str,
    name: &str,
    year: &str,
    poster: &str,
) -> Option<MetaPreview> {
    matches(query, name, year).then(|| {
        document(
            MetaPreview {
                id: id.into(),
                type_: kind.into(),
                name: name.into(),
                poster: (!poster.is_empty()).then(|| poster.into()),
                ..Default::default()
            },
            id.into(),
            name.into(),
            "Saved",
            year.into(),
            Action::Title,
        )
    })
}

pub(super) fn replace_saved_documents(
    local: &ProjectionLocal,
    saved: impl Iterator<Item = MetaPreview>,
    membership: &HashSet<(String, String)>,
) -> ProjectionLocal {
    let mut results = ScopedResults::default();
    for meta in saved {
        results.insert(meta, true);
    }
    for meta in local
        .results
        .iter()
        .filter(|meta| !matches!(action(meta), Some(Action::Title)))
    {
        results.insert(
            meta.clone(),
            membership.contains(&(meta.type_.clone(), title_id(meta).to_owned())),
        );
    }
    ProjectionLocal {
        scope: local.scope,
        results: results.finish().into(),
    }
}

#[cfg(test)]
pub(super) fn merge(state: &Shared, remote: Vec<MetaPreview>) -> Vec<MetaPreview> {
    let saved = state
        .entries
        .iter()
        .map(|entry| (entry.type_.clone(), entry.id.clone()))
        .collect();
    merge_plain(&state.unified_search.projection_local(), &saved, remote)
}

pub(super) fn merge_plain(
    local: &ProjectionLocal,
    saved: &HashSet<(String, String)>,
    remote: Vec<MetaPreview>,
) -> Vec<MetaPreview> {
    let mut results: Vec<_> = local
        .results
        .iter()
        .filter(|meta| match (local.scope, action(meta)) {
            (0, _) => true,
            (2, Some(Action::Title)) => true,
            (2, Some(Action::Download { .. })) => {
                saved.contains(&(meta.type_.clone(), title_id(meta).to_owned()))
            }
            (3, Some(Action::Episode { .. }))
            | (4, Some(Action::Download { .. }))
            | (5, Some(Action::Bookmark { .. })) => true,
            _ => false,
        })
        .take(LIMIT)
        .cloned()
        .collect();
    if local.scope <= 1 {
        let local_titles: HashSet<_> = results
            .iter()
            .filter(|meta| matches!(action(meta), Some(Action::Title | Action::Download { .. })))
            .map(|meta| (meta.type_.clone(), title_id(meta).to_owned()))
            .collect();
        results.extend(
            remote
                .into_iter()
                .filter(|meta| {
                    !global_id(&meta.id)
                        || !local_titles.contains(&(meta.type_.clone(), meta.id.clone()))
                })
                .map(|mut meta| {
                    // This trusted caption belongs only to the search projection. Do
                    // not turn a catalog hit into a local action or rewrite its ID.
                    // Library entries lack addon ownership, so an opaque spelling
                    // cannot prove membership across source-scoped catalog hits.
                    meta.extra.remove(LABEL);
                    if global_id(&meta.id) && saved.contains(&(meta.type_.clone(), meta.id.clone()))
                    {
                        meta.extra.insert(LABEL.into(), "Saved".into());
                    }
                    meta
                }),
        );
    }
    results
}

pub(super) fn card_text(meta: &MetaPreview) -> (String, String) {
    let label = meta
        .extra
        .get(LABEL)
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let context = meta
        .extra
        .get(CONTEXT)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| meta.year_str().unwrap_or_default());
    (text::tr(label).into(), context)
}

impl Bridge {
    pub(super) fn enqueue_search_projection(&self, request: catalog::SearchProjectionRequest) {
        let work = self.shared.lock().unwrap().unified_search.work.clone();
        let (start, retired) = {
            let mut pending = work.lock().unwrap();
            let retired = pending.projection.submit(request);
            let start = !pending.running;
            pending.running = true;
            (start, retired)
        };
        drop(retired);
        if start {
            #[cfg(test)]
            search_test_thread_started();
            let bridge = self.clone();
            thread::spawn(move || bridge.run_local_search_worker(work));
        }
    }
    fn service_search_projection(&self, work: &Arc<Mutex<SearchWork>>) {
        // One foreground batch per safe boundary keeps an arrival stream from
        // starving the required corpus scan. New pages coalesce for the next.
        let task = work.lock().unwrap().projection.take();
        if let Some(task) = task {
            self.process_search_projection(task, work.clone());
        }
    }
    /// Library is published for progress updates too. Borrowed membership checks
    /// keep those unchanged publications free of corpus rebuilds and workers.
    pub(super) fn refresh_unified_search_library(&self) {
        let Some(app) = self.app() else {
            return;
        };
        if !app.get_discover_search_open() {
            return;
        }
        let (query, saved_changed, needed) = {
            let state = self.shared.lock().unwrap();
            state.unified_search.clock.progress(&state.progress, false);
            state.unified_search.clock.visibility(&state.cache_settings);
            let saved_changed = !state.unified_search.library_matches(&state.entries);
            if saved_changed {
                state.unified_search.clock.advance(CorpusDomain::Saved);
            }
            let needed = saved_changed
                || state.unified_search.requested_semantic != state.unified_search.clock.revision();
            (state.unified_search.query.clone(), saved_changed, needed)
        };
        if needed {
            self.refresh_local_search_inner(&query, saved_changed);
        }
    }

    pub(super) fn refresh_unified_search_presentation(&self) {
        if let Some(app) = self.app()
            && app.get_discover_search_open()
        {
            self.refresh_local_search(&app.get_search_text());
        }
    }

    pub(super) fn refresh_local_search(&self, query: &str) {
        self.refresh_local_search_inner(query, false);
    }

    /// The UI tick reads counters/time only. Filesystem observation stays on
    /// the same bounded lane, and a full query always supersedes a probe.
    pub(super) fn refresh_unified_search_downloads(&self) {
        if !self.app().is_some_and(|app| app.get_discover_search_open()) {
            return;
        }
        let (changed, probe, work, instance, query) = {
            let mut state = self.shared.lock().unwrap();
            if state.unified_search.query.chars().count() < 2 {
                return;
            }
            let revision = self.downloads.semantic_revision();
            let changed = state.unified_search.downloads_seen != revision;
            if changed {
                state.unified_search.downloads_seen = revision;
                state.unified_search.clock.advance(CorpusDomain::Downloads);
            }
            let now = std::time::Instant::now();
            let probe = !changed
                && state
                    .unified_search
                    .last_availability_probe
                    .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(1));
            if probe {
                state.unified_search.last_availability_probe = Some(now);
            }
            (
                changed,
                probe,
                state.unified_search.work.clone(),
                state.unified_search.instance,
                state.unified_search.query.clone(),
            )
        };
        if changed {
            let active = ACTIVE_CORPUS.lock().unwrap().clone();
            if let Some(active) = active.filter(|active| active.instance == instance) {
                post_corpus_refresh(&active);
            }
        } else if probe {
            let start = work.lock().unwrap().submit_probe(
                profiles::capture(),
                self.clone(),
                instance,
                &query,
            );
            if start {
                #[cfg(test)]
                {
                    search_test_pending(true);
                    search_test_thread_started();
                }
                let bridge = self.clone();
                thread::spawn(move || bridge.run_local_search_worker(work));
            }
        }
    }

    fn refresh_local_search_inner(&self, query: &str, retain_documents: bool) {
        let context = profiles::capture();
        if !profiles::is_current(&context) {
            return;
        }
        let query = query.trim().to_owned();
        let (work, stamp) = {
            let mut state = self.shared.lock().unwrap();
            if retain_documents {
                state.unified_search.corpus_revision =
                    state.unified_search.corpus_revision.wrapping_add(1);
            }
            state.unified_search.clock.progress(&state.progress, false);
            state.unified_search.clock.visibility(&state.cache_settings);
            *ACTIVE_CORPUS.lock().unwrap() = Some(RegisteredCorpus {
                owner: profiles::ProfileStore::capture(),
                profile: context.id.clone(),
                epoch: context.epoch,
                instance: state.unified_search.instance,
                clock: state.unified_search.clock.clone(),
                bridge: self.clone(),
            });
            (
                state.unified_search.work.clone(),
                CorpusStamp::capture(
                    state.unified_search.corpus_revision,
                    state.unified_search.instance,
                    state.unified_search.clock.clone(),
                    self.downloads.clone(),
                ),
            )
        };
        let semantic = stamp.semantic;
        let download_revision = stamp.download_revision;
        let submitted = {
            let mut pending = work.lock().unwrap();
            let submitted = pending.submit(SearchReceipt {
                context,
                query: query.clone(),
                stamp,
            });
            if submitted.is_some()
                && let Some(latest) = pending.latest.as_mut()
            {
                latest.bridge = Some(self.clone());
            }
            #[cfg(test)]
            search_test_pending(pending.latest.is_some());
            submitted
        };
        let Some((revision, start)) = submitted else {
            // The settled network request can clear its model while an identical
            // local query already has cached results or a pending publication.
            self.repaint_search();
            return;
        };
        {
            let mut state = self.shared.lock().unwrap();
            state.unified_search.query = query.clone();
            state.unified_search.revision = revision;
            state.unified_search.requested_semantic = semantic;
            state.unified_search.downloads_seen = download_revision;
            state.unified_search.last_availability_probe = Some(std::time::Instant::now());
            let Shared {
                entries,
                unified_search,
                ..
            } = &mut *state;
            unified_search.remember_library(entries);
            if !retain_documents {
                unified_search.results = Arc::default();
                unified_search.results_stamp = None;
            }
        }
        if retain_documents {
            self.repaint_search_memory();
        } else {
            self.repaint_search();
        }
        if start {
            #[cfg(test)]
            search_test_thread_started();
            let bridge = self.clone();
            thread::spawn(move || bridge.run_local_search_worker(work));
        }
    }

    fn local_search_snapshot(
        &self,
        current: &impl Fn() -> bool,
    ) -> Option<(Vec<LibraryEntry>, Vec<MetaPreview>, SpoilerSnapshot)> {
        if !current() {
            return None;
        }
        let revealed = spoiler::revealed_snapshot(&profiles::capture());
        let state = self.shared.lock().unwrap();
        state.unified_search.clock.progress(&state.progress, false);
        state.unified_search.clock.visibility(&state.cache_settings);
        if !current() {
            return None;
        }
        let mut entries = Vec::with_capacity(state.entries.len());
        for (index, entry) in state.entries.iter().enumerate() {
            if index % 64 == 0 && !current() {
                return None;
            }
            entries.push(entry.clone());
        }
        let mut known = Vec::with_capacity(state.previews.len());
        for (index, meta) in state.previews.iter().enumerate() {
            if index % 64 == 0 && !current() {
                return None;
            }
            known.push(meta.clone());
        }
        let mut watched = HashSet::new();
        for (index, (key, progress)) in state.progress.iter().enumerate() {
            if index % 64 == 0 && !current() {
                return None;
            }
            if progress.watched {
                watched.insert(key.clone());
            }
            if let Some(history) = &progress.history
                && !history.type_.is_empty()
                && !history.title.is_empty()
            {
                known.push(MetaPreview {
                    id: progress.series_id.clone(),
                    type_: history.type_.clone(),
                    name: history.title.clone(),
                    poster: (!history.poster_url.is_empty()).then(|| history.poster_url.clone()),
                    ..Default::default()
                });
            }
        }
        let spoilers = SpoilerSnapshot {
            protected: state.cache_settings.spoiler_protection,
            watched,
            revealed,
        };
        current().then_some((entries, known, spoilers))
    }

    fn retry_local_search_receipt(&self, work: Arc<Mutex<SearchWork>>, request: SearchRequest) {
        let bridge = self.clone();
        let failed_work = work.clone();
        let failed_request = request.clone();
        let queued = profiles::invoke_from_event_loop(move || {
            // A changing inventory should eventually refresh the final query,
            // without spinning a worker while metadata is still being written.
            slint::Timer::single_shot(Duration::from_millis(25), move || {
                if profiles::is_current(&request.receipt.context)
                    && work.lock().unwrap().current(&request)
                    && !request
                        .receipt
                        .stamp
                        .clock
                        .facts
                        .lock()
                        .unwrap()
                        .admitted_progress
                {
                    bridge.refresh_local_search(&request.receipt.query);
                }
            });
        });
        if queued.is_err() {
            failed_work.lock().unwrap().abandon(&failed_request);
        }
    }

    fn run_local_search_worker(&self, work: Arc<Mutex<SearchWork>>) {
        loop {
            let Some(task) = work.lock().unwrap().take_task() else {
                return;
            };
            let request = match task {
                SearchTask::Projection(task) => {
                    self.process_search_projection(*task, work.clone());
                    continue;
                }
                SearchTask::Corpus(request) => *request,
            };
            #[cfg(test)]
            let _scan = search_test_scan_started(request.kind);
            let scan_bridge = request.bridge.as_ref().unwrap_or(self);
            profiles::with_context(request.context.clone(), || {
                let current = || {
                    profiles::is_current(&request.receipt.context)
                        && request.receipt.stamp.current()
                        && work.lock().unwrap().current(&request)
                };
                if request.kind == SearchRequestKind::Availability {
                    let changed = scan_bridge
                        .downloads
                        .observe_completed_artifacts(&profiles::ProfileStore::capture(), current);
                    if changed
                        && profiles::is_current(&request.receipt.context)
                        && work.lock().unwrap().current(&request)
                    {
                        request.receipt.stamp.clock.advance(CorpusDomain::Downloads);
                        let active = ACTIVE_CORPUS.lock().unwrap().clone();
                        if let Some(active) = active
                            .filter(|active| active.instance == request.receipt.stamp.instance)
                        {
                            post_corpus_refresh(&active);
                        }
                    } else if !current()
                        && profiles::is_current(&request.receipt.context)
                        && work.lock().unwrap().current(&request)
                    {
                        scan_bridge.retry_local_search_receipt(work.clone(), request.clone());
                    }
                    return;
                }
                let results = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    scan_bridge
                        .downloads
                        .observe_completed_artifacts(&profiles::ProfileStore::capture(), current);
                    let (entries, known, spoilers) = scan_bridge.local_search_snapshot(&current)?;
                    if !current() {
                        return None;
                    }
                    collect_results_current(
                        &request.receipt.query,
                        &entries,
                        &known,
                        scan_bridge.downloads.jobs(),
                        &spoilers,
                        current,
                        || scan_bridge.service_search_projection(&work),
                    )
                }))
                .ok()
                .flatten();
                let results = results.and_then(|mut results| {
                    let policy = profile_policy::TitlePolicySnapshot::capture_browse(
                        &profiles::ProfileStore::capture(),
                    )
                    .ok()?;
                    results.retain(|meta| policy.allows(&meta.type_, title_id(meta)));
                    Some(results)
                });
                let Some(results) = results.filter(|_| current()) else {
                    if profiles::is_current(&request.receipt.context)
                        && work.lock().unwrap().current(&request)
                        && !request.receipt.stamp.current()
                    {
                        scan_bridge.retry_local_search_receipt(work.clone(), request.clone());
                    } else {
                        work.lock().unwrap().abandon(&request);
                    }
                    return;
                };
                let bridge = scan_bridge.clone();
                let callback_work = work.clone();
                let callback_request = request.clone();
                let queued = profiles::invoke_from_event_loop(move || {
                    if !callback_work.lock().unwrap().current(&callback_request) {
                        return;
                    }
                    if !callback_request.receipt.stamp.current() {
                        bridge.retry_local_search_receipt(callback_work, callback_request);
                        return;
                    }
                    {
                        let mut state = bridge.shared.lock().unwrap();
                        if state.unified_search.revision != callback_request.revision
                            || state.unified_search.query != callback_request.receipt.query
                        {
                            return;
                        }
                        #[cfg(test)]
                        search_test_published(&callback_request, &results);
                        state.unified_search.results = results.into();
                        state.unified_search.results_stamp =
                            Some(callback_request.receipt.stamp.clone());
                    }
                    bridge.repaint_search();
                });
                if queued.is_err() {
                    work.lock().unwrap().abandon(&request);
                }
            });
        }
    }

    pub(super) fn pick_search_scope(&self, scope: i32) {
        let scope = scope.clamp(0, 5);
        {
            let mut state = self.shared.lock().unwrap();
            if state.unified_search.scope == scope {
                return;
            }
            state.unified_search.scope = scope;
        }
        if let Some(app) = self.app() {
            app.set_search_scope(scope);
            // Detail Back preserves its painted cards without replaying their
            // entrance. An explicit new scope is a new result view instead.
            app.set_discover_search_animate_results(true);
            app.set_discover_kb_idx(0);
            app.set_discover_kb_zone(2);
            app.set_discover_kb_ctl(4);
            app.set_discover_search_scroll_y(0.0);
        }
        self.repaint_search();
    }

    pub(super) fn open_search_result(
        &self,
        meta: &MetaPreview,
        generation: u64,
        index: usize,
    ) -> bool {
        let Some(action) = action(meta) else {
            if !global_id(&meta.id)
                && let Some(source) = meta
                    .extra
                    .get("novaSourceUrl")
                    .and_then(|v| v.as_str())
                    .filter(|source| !source.is_empty())
            {
                self.open_catalog_detail(
                    meta.clone(),
                    nova_providers::catalog_grouping::SourceIdentity {
                        addon_url: source.into(),
                        media_type: meta.type_.clone(),
                        id: meta.id.clone(),
                    },
                    None,
                    generation,
                    Some(index),
                    false,
                );
                return true;
            }
            return false;
        };
        if profile_policy::entry_blocked()
            || !profile_policy::allows_browse_title(
                &profiles::ProfileStore::capture(),
                &meta.type_,
                title_id(meta),
            )
        {
            return true;
        }
        match action {
            Action::Title => return false,
            Action::Download { job, .. } => {
                if self
                    .downloads
                    .job(&job)
                    .is_some_and(|job| usable(&job, &profiles::ProfileStore::capture()))
                {
                    self.play_download(&job);
                } else {
                    let query = {
                        let mut state = self.shared.lock().unwrap();
                        // Preserve this explicit unusable-download refresh even
                        // when the text and other corpus receipts are unchanged.
                        state.unified_search.corpus_revision =
                            state.unified_search.corpus_revision.wrapping_add(1);
                        state.unified_search.query.clone()
                    };
                    self.refresh_local_search(&query);
                }
            }
            Action::Bookmark { scene, .. } => self.play_saved_scene(&scene),
            Action::Episode { title, video } => {
                let mut preview = meta.clone();
                preview.id = title.clone();
                preview.name = meta
                    .extra
                    .get(CONTEXT)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .into();
                preview.extra.remove(ACTION);
                self.open_preview(preview, 0, true, None, false);
                {
                    let mut state = self.shared.lock().unwrap();
                    if let Some(modal) = state
                        .modal_item
                        .as_mut()
                        .filter(|modal| modal.id == title && modal.type_ == meta.type_)
                        && !modal.videos.iter().any(|existing| existing.id == video.id)
                    {
                        modal.videos.push(video.clone());
                    }
                }
                self.select_episode_streams(title, video.id.clone(), video, 0);
                if let Some(app) = self.app() {
                    app.set_detail_deep_stream(true);
                }
            }
        }
        true
    }
}

#[cfg(test)]
type SearchTestReader = dyn Fn(&profiles::Context, &str, &str) -> Vec<Video> + Send + Sync;

#[cfg(test)]
#[derive(Debug)]
struct SearchTestPublication {
    profile: String,
    epoch: u64,
    query: String,
    ids: Vec<String>,
}

#[cfg(test)]
struct SearchTestProbe {
    reader: Arc<SearchTestReader>,
    active: AtomicUsize,
    peak_active: AtomicUsize,
    peak_pending: AtomicUsize,
    threads: AtomicUsize,
    corpus_scans: AtomicUsize,
    availability_scans: AtomicUsize,
    publications: Mutex<Vec<SearchTestPublication>>,
}

#[cfg(test)]
static SEARCH_TEST_PROBE: Mutex<Option<Arc<SearchTestProbe>>> = Mutex::new(None);

#[cfg(test)]
struct SearchTestScope;

#[cfg(test)]
impl SearchTestScope {
    fn install(probe: Arc<SearchTestProbe>) -> Self {
        assert!(SEARCH_TEST_PROBE.lock().unwrap().replace(probe).is_none());
        Self
    }
}

#[cfg(test)]
impl Drop for SearchTestScope {
    fn drop(&mut self) {
        SEARCH_TEST_PROBE.lock().unwrap().take();
    }
}

#[cfg(test)]
struct SearchTestScan(Arc<SearchTestProbe>);

#[cfg(test)]
impl Drop for SearchTestScan {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
fn search_test_scan_started(kind: SearchRequestKind) -> Option<SearchTestScan> {
    let probe = SEARCH_TEST_PROBE.lock().unwrap().clone()?;
    match kind {
        SearchRequestKind::Corpus => {
            probe.corpus_scans.fetch_add(1, Ordering::AcqRel);
        }
        SearchRequestKind::Availability => {
            probe.availability_scans.fetch_add(1, Ordering::AcqRel);
        }
    }
    let active = probe.active.fetch_add(1, Ordering::AcqRel) + 1;
    probe.peak_active.fetch_max(active, Ordering::AcqRel);
    Some(SearchTestScan(probe))
}

#[cfg(test)]
fn search_test_read_inventory(kind: &str, id: &str) -> Option<Vec<Video>> {
    let probe = SEARCH_TEST_PROBE.lock().unwrap().clone()?;
    Some((probe.reader)(&profiles::capture(), kind, id))
}

#[cfg(test)]
fn search_test_pending(pending: bool) {
    if let Some(probe) = SEARCH_TEST_PROBE.lock().unwrap().clone() {
        probe
            .peak_pending
            .fetch_max(usize::from(pending), Ordering::AcqRel);
    }
}

#[cfg(test)]
fn search_test_thread_started() {
    if let Some(probe) = SEARCH_TEST_PROBE.lock().unwrap().clone() {
        probe.threads.fetch_add(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
fn search_test_published(request: &SearchRequest, results: &[MetaPreview]) {
    if let Some(probe) = SEARCH_TEST_PROBE.lock().unwrap().clone() {
        probe
            .publications
            .lock()
            .unwrap()
            .push(SearchTestPublication {
                profile: request.receipt.context.id.clone(),
                epoch: request.receipt.context.epoch,
                query: request.receipt.query.clone(),
                ids: results
                    .iter()
                    .map(|meta| title_id(meta).to_owned())
                    .collect(),
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_progress_receipt_ignores_byte_checkpoints_and_settles_admission() {
        let clock = CorpusClock::default();
        let mut rows = HashMap::from([(
            "episode".into(),
            EpisodeProgress {
                series_id: "parent".into(),
                history: Some(PlaybackHistory {
                    type_: "series".into(),
                    title: "Title".into(),
                    poster_url: "poster".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )]);
        assert!(!clock.progress(&rows, false));
        for checkpoint in 0..100 {
            let row = rows.get_mut("episode").unwrap();
            row.position_secs = checkpoint as f64;
            row.duration_secs = 900.0 + checkpoint as f64;
            row.updated_at_secs = checkpoint;
            let prepared = prepare_snapshot_change(
                EPISODE_PROGRESS_KEY,
                &serde_json::to_string(&rows).unwrap(),
            )
            .unwrap();
            let PreparedSnapshotFacts::Progress(facts) = prepared.change else {
                panic!("progress facts")
            };
            assert!(!clock.committed_progress(&facts));
        }
        assert_eq!(clock.revision(), 0);
        rows.get_mut("episode").unwrap().watched = true;
        assert!(clock.progress(&rows, true));
        let prepared =
            prepare_snapshot_change(EPISODE_PROGRESS_KEY, &serde_json::to_string(&rows).unwrap())
                .unwrap();
        let PreparedSnapshotFacts::Progress(facts) = prepared.change else {
            panic!("progress facts")
        };
        assert!(
            clock.committed_progress(&facts),
            "accepted semantic change posts after durable success"
        );
        assert!(!clock.committed_progress(&facts));
        assert_eq!(clock.revision(), 1);
    }

    #[test]
    fn prepared_snapshot_resolves_current_complete_owner_at_commit() {
        super::super::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::prepared_snapshot_resolves_current_complete_owner_at_commit"
            ),
            |fixture| {
                let bridge = &fixture.bridge;
                bridge.app().unwrap().set_discover_search_open(true);
                bridge.refresh_local_search("N");
                let owner = profiles::ProfileStore::capture();
                let old = ACTIVE_CORPUS.lock().unwrap().as_ref().unwrap().clone();
                let prepared = prepare_snapshot_change(
                    "library",
                    r#"[{"id":"tt1","type_":"movie","name":"New durable"}]"#,
                )
                .unwrap();
                let fresh = Arc::new(CorpusClock::default());
                *ACTIVE_CORPUS.lock().unwrap() = Some(RegisteredCorpus {
                    clock: fresh.clone(),
                    ..old.clone()
                });
                let other = profiles::create("Other durable owner").unwrap();
                assert!(
                    commit_snapshot_change(&profiles::ProfileStore::for_profile(&other), &prepared)
                        .is_none()
                );
                assert_eq!(fresh.revision(), 0);
                assert!(commit_snapshot_change(&owner, &prepared).is_some());
                assert_eq!(fresh.revision(), 1);
                assert_eq!(
                    old.clock.revision(),
                    0,
                    "preparation must not retain old activation clock"
                );
                assert!(
                    commit_snapshot_change(&owner, &prepared).is_none(),
                    "unchanged commit is silent"
                );
            },
        );
    }

    #[test]
    fn collector_snapshot_cannot_consume_pending_ui_membership_publication() {
        super::super::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::collector_snapshot_cannot_consume_pending_ui_membership_publication"
            ),
            |fixture| {
                let bridge = &fixture.bridge;
                {
                    let mut state = bridge.shared.lock().unwrap();
                    state.unified_search.remember_library(&[]);
                    state.entries = serde_json::from_value(serde_json::json!([
                        {"id":"admitted", "type_":"movie", "name":"Admitted title"}
                    ]))
                    .unwrap();
                    assert!(!state.unified_search.library_matches(&state.entries));
                }
                let (captured, _, _) = bridge.local_search_snapshot(&|| true).unwrap();
                assert_eq!(captured.len(), 1);
                let state = bridge.shared.lock().unwrap();
                assert!(
                    !state.unified_search.library_matches(&state.entries),
                    "a collector may read the edit but cannot mark its UI publication handled"
                );
            },
        );
    }

    #[test]
    fn semantic_progress_receipt_tracks_watched_and_history_facts_only() {
        let clock = CorpusClock::default();
        let mut rows = HashMap::from([(
            "episode".into(),
            EpisodeProgress {
                series_id: "parent".into(),
                history: Some(PlaybackHistory {
                    type_: "series".into(),
                    title: "Title".into(),
                    poster_url: "poster".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )]);
        assert!(!clock.progress(&rows, false));
        for checkpoint in 0..100 {
            let row = rows.get_mut("episode").unwrap();
            row.position_secs = checkpoint as f64;
            row.duration_secs = checkpoint as f64 + 1000.0;
            row.updated_at_secs = checkpoint;
            row.history.as_mut().unwrap().played_at_secs = checkpoint;
            assert!(!clock.progress(&rows, false));
        }
        assert_eq!(clock.revision(), 0);
        for field in 0..5 {
            let row = rows.get_mut("episode").unwrap();
            match field {
                0 => row.watched = true,
                1 => row.series_id = "new parent".into(),
                2 => row.history.as_mut().unwrap().type_ = "anime".into(),
                3 => row.history.as_mut().unwrap().title = "New title".into(),
                _ => row.history.as_mut().unwrap().poster_url = "new poster".into(),
            }
            assert!(clock.progress(&rows, true));
            assert!(!clock.progress(&rows, true));
        }
        assert_eq!(clock.revision(), 5);
    }

    fn semantic_probe(reader: Arc<SearchTestReader>) -> Arc<SearchTestProbe> {
        Arc::new(SearchTestProbe {
            reader,
            active: AtomicUsize::new(0),
            peak_active: AtomicUsize::new(0),
            peak_pending: AtomicUsize::new(0),
            threads: AtomicUsize::new(0),
            corpus_scans: AtomicUsize::new(0),
            availability_scans: AtomicUsize::new(0),
            publications: Mutex::new(Vec::new()),
        })
    }

    fn settle_semantic_search(_bridge: &Bridge) {
        let until = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(30));
            profiles::invoke_account(|| slint::quit_event_loop().unwrap()).unwrap();
            slint::run_event_loop().unwrap();
            if !LOCAL_SEARCH_WORK.lock().unwrap().running && !CORPUS_REFRESH.lock().unwrap().posted
            {
                // Worker ownership ends before its UI publication is delivered.
                profiles::invoke_account(|| slint::quit_event_loop().unwrap()).unwrap();
                slint::run_event_loop().unwrap();
                if !LOCAL_SEARCH_WORK.lock().unwrap().running
                    && !CORPUS_REFRESH.lock().unwrap().posted
                {
                    return;
                }
            }
            assert!(std::time::Instant::now() < until, "search did not settle");
            std::thread::yield_now();
        }
    }

    #[test]
    fn projection_page_services_at_corpus_boundary_after_blocking_read() {
        super::super::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::projection_page_services_at_corpus_boundary_after_blocking_read"
            ),
            |fixture| {
                let bridge = &fixture.bridge;
                let app = bridge.app().unwrap();
                {
                    let mut state = bridge.shared.lock().unwrap();
                    state.entries = serde_json::from_value(serde_json::json!([
                        {"id":"first", "type_":"series", "name":"Needle first"},
                        {"id":"second", "type_":"series", "name":"Needle second"}
                    ]))
                    .unwrap();
                    state.search = "Needle".into();
                    state.search_generation = 93;
                    state.search_pending_requests = 1;
                    state.search_targets = vec![SearchTarget {
                        addon_url: "https://fixture.invalid".into(),
                        type_: "movie".into(),
                        catalog_id: "search".into(),
                        genre_options: vec![],
                        genre: String::new(),
                        supports_skip: false,
                        next_skip: 0,
                        exhausted: false,
                        page_fingerprints: HashSet::new(),
                        results: Arc::default(),
                    }];
                }
                let (entered, waiting) = std::sync::mpsc::sync_channel(2);
                let (release, blocked) = std::sync::mpsc::sync_channel(2);
                let blocked = Mutex::new(blocked);
                let reads = AtomicUsize::new(0);
                let probe = semantic_probe(Arc::new(move |_, _, _| {
                    if reads.fetch_add(1, Ordering::AcqRel) < 2 {
                        entered.send(()).unwrap();
                        blocked
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(30))
                            .unwrap();
                    }
                    Vec::new()
                }));
                let _scope = SearchTestScope::install(probe.clone());
                app.set_search_text("Needle".into());
                app.set_discover_search_open(true);
                bridge.refresh_local_search("Needle");
                waiting.recv_timeout(Duration::from_secs(15)).unwrap();
                let admitted = std::time::Instant::now();
                bridge.search_test_admit_page(
                    93,
                    vec![MetaPreview {
                        id: "tt9900990".into(),
                        type_: "movie".into(),
                        name: "Needle remote".into(),
                        ..Default::default()
                    }],
                );
                assert_eq!(probe.threads.load(Ordering::Acquire), 1);
                assert!(
                    bridge
                        .shared
                        .lock()
                        .unwrap()
                        .search_previews
                        .iter()
                        .all(|row| row.id != "tt9900990")
                );
                let released = std::time::Instant::now();
                release.send(()).unwrap();
                // The second inventory read remains blocked: publication must
                // occur at the preceding safe boundary, before corpus completion.
                waiting.recv_timeout(Duration::from_secs(15)).unwrap();
                let until = std::time::Instant::now() + Duration::from_secs(15);
                loop {
                    profiles::invoke_account(|| slint::quit_event_loop().unwrap()).unwrap();
                    slint::run_event_loop().unwrap();
                    if bridge
                        .shared
                        .lock()
                        .unwrap()
                        .search_previews
                        .iter()
                        .any(|row| row.id == "tt9900990")
                    {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < until,
                        "remote projection waited for whole corpus"
                    );
                    std::thread::yield_now();
                }
                eprintln!(
                    "UI05_BOUNDARY admission_to_first_us={} blocked_read_residual_us={} release_to_first_us={}",
                    admitted.elapsed().as_micros(),
                    released.duration_since(admitted).as_micros(),
                    released.elapsed().as_micros()
                );
                assert!(probe.publications.lock().unwrap().is_empty());
                assert_eq!(probe.peak_active.load(Ordering::Acquire), 1);
                assert_eq!(probe.threads.load(Ordering::Acquire), 1);
                release.send(()).unwrap();
                settle_semantic_search(bridge);
                assert!(
                    bridge
                        .shared
                        .lock()
                        .unwrap()
                        .search_previews
                        .iter()
                        .any(|row| row.id == "tt9900990")
                );
                assert_eq!(probe.corpus_scans.load(Ordering::Acquire), 1);
            },
        );
    }

    #[test]
    fn local_search_semantic_commits_refresh_retained_results_and_ignore_checkpoints() {
        super::super::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::local_search_semantic_commits_refresh_retained_results_and_ignore_checkpoints"
            ),
            |fixture| {
                let bridge = &fixture.bridge;
                let app = bridge.app().unwrap();
                let probe = semantic_probe(Arc::new(|_, _, _| Vec::new()));
                let _scope = SearchTestScope::install(probe.clone());
                bridge.shared.lock().unwrap().entries = serde_json::from_value(serde_json::json!([
                    {"id":"same", "type_":"movie", "name":"Needle old", "year":"2020", "poster_url":"old"}
                ])).unwrap();
                app.set_discover_search_open(true);
                bridge.refresh_local_search("Needle");
                settle_semantic_search(bridge);
                assert!(!probe.publications.lock().unwrap().is_empty());
                {
                    let mut state = bridge.shared.lock().unwrap();
                    state.entries[0].name = "Needle corrected".into();
                    state.entries[0].year = "2026".into();
                    state.entries[0].poster_url = "corrected".into();
                }
                bridge.persist_library();
                persistence::flush().unwrap();
                settle_semantic_search(bridge);
                {
                    let state = bridge.shared.lock().unwrap();
                    let hit = state
                        .unified_search
                        .results
                        .iter()
                        .find(|hit| matches!(action(hit), Some(Action::Title)))
                        .unwrap();
                    assert_eq!(hit.name, "Needle corrected");
                    assert_eq!(hit.poster.as_deref(), Some("corrected"));
                    assert_eq!(
                        hit.extra.get(CONTEXT).and_then(|value| value.as_str()),
                        Some("2026")
                    );
                }
                let count = probe.corpus_scans.load(Ordering::Acquire);
                for checkpoint in 0..100 {
                    let rows = HashMap::from([(
                        "same:episode".into(),
                        EpisodeProgress {
                            series_id: "same".into(),
                            episode_id: "episode".into(),
                            position_secs: checkpoint as f64,
                            duration_secs: 1000.0,
                            updated_at_secs: checkpoint,
                            ..Default::default()
                        },
                    )]);
                    bridge.shared.lock().unwrap().progress = rows.clone();
                    // Exercise accepted admission and the successful durable path.
                    progress_admitted(&profiles::ProfileStore::capture(), &rows);
                    sync::persist_sync_snapshot(
                        EPISODE_PROGRESS_KEY,
                        &serde_json::to_string(&rows).unwrap(),
                        false,
                    )
                    .unwrap();
                    bridge.refresh_unified_search_library();
                }
                settle_semantic_search(bridge);
                assert_eq!(probe.corpus_scans.load(Ordering::Acquire), count);
                let revision = bridge
                    .shared
                    .lock()
                    .unwrap()
                    .unified_search
                    .clock
                    .revision();
                assert!(
                    sync::persist_sync_snapshot(bookmarks::BOOKMARKS_KEY, "invalid json", false)
                        .is_err()
                );
                let other = profiles::create("Inactive search owner").unwrap();
                profiles::ProfileStore::for_profile(&other)
                    .persist_snapshot(bookmarks::BOOKMARKS_KEY, "{}", false)
                    .unwrap();
                assert_eq!(
                    bridge
                        .shared
                        .lock()
                        .unwrap()
                        .unified_search
                        .clock
                        .revision(),
                    revision
                );
                assert!(!CORPUS_REFRESH.lock().unwrap().posted);
                let key = serde_json::to_string(&("movie", "same", "same")).unwrap();
                for note in [Some("Needle first note"), Some("Needle edited note"), None] {
                    let rows: bookmarks::Bookmarks = note
                        .map(|note| {
                            HashMap::from([(
                                key.clone(),
                                vec![bookmarks::Bookmark {
                                    id: "scene".into(),
                                    position: 42.0,
                                    note: note.into(),
                                    added_at: 1,
                                    metadata: None,
                                }],
                            )])
                        })
                        .unwrap_or_default();
                    // Remote projection uses this same committed snapshot entrypoint.
                    sync::persist_sync_snapshot(
                        bookmarks::BOOKMARKS_KEY,
                        &serde_json::to_string(&rows).unwrap(),
                        false,
                    )
                    .unwrap();
                    settle_semantic_search(bridge);
                    let state = bridge.shared.lock().unwrap();
                    let notes: Vec<_> = state
                        .unified_search
                        .results
                        .iter()
                        .filter(|hit| matches!(action(hit), Some(Action::Bookmark { .. })))
                        .map(|hit| hit.name.as_str())
                        .collect();
                    assert_eq!(notes, note.into_iter().collect::<Vec<_>>());
                }
                assert_eq!(probe.peak_active.load(Ordering::Acquire), 1);
            },
        );
    }

    #[test]
    fn local_search_semantic_commit_cancels_paused_collector_before_publication() {
        super::super::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::local_search_semantic_commit_cancels_paused_collector_before_publication"
            ),
            |fixture| {
                let bridge = &fixture.bridge;
                let app = bridge.app().unwrap();
                bridge.shared.lock().unwrap().entries = serde_json::from_value(serde_json::json!([
                    {"id":"race", "type_":"series", "name":"Needle old"}
                ]))
                .unwrap();
                let (entered, waiting) = std::sync::mpsc::sync_channel(1);
                let (release, blocked) = std::sync::mpsc::sync_channel(1);
                let blocked = Mutex::new(blocked);
                let once = AtomicBool::new(true);
                bridge
                    .shared
                    .lock()
                    .unwrap()
                    .cache_settings
                    .spoiler_protection = false;
                let probe = semantic_probe(Arc::new(move |_, _, _| {
                    if once.swap(false, Ordering::AcqRel) {
                        entered.send(()).unwrap();
                        blocked
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(30))
                            .unwrap();
                    }
                    vec![Video {
                        id: "race:1:1".into(),
                        name: "Secret episode title".into(),
                        season: Some(1),
                        episode: Some(1),
                        ..Default::default()
                    }]
                }));
                let _scope = SearchTestScope::install(probe.clone());
                app.set_discover_search_open(true);
                bridge.refresh_local_search("Needle");
                waiting.recv_timeout(Duration::from_secs(15)).unwrap();
                bridge.shared.lock().unwrap().entries[0].name = "Needle current".into();
                let settings = {
                    let mut state = bridge.shared.lock().unwrap();
                    state.cache_settings.spoiler_protection = true;
                    state.cache_settings.clone()
                };
                bridge.persist_library();
                persistence::flush().unwrap();
                sync::persist_sync_snapshot(
                    "settings",
                    &serde_json::to_string(&settings).unwrap(),
                    false,
                )
                .unwrap();
                assert!(probe.publications.lock().unwrap().is_empty());
                release.send(()).unwrap();
                settle_semantic_search(bridge);
                let state = bridge.shared.lock().unwrap();
                assert!(
                    state
                        .unified_search
                        .results
                        .iter()
                        .any(|hit| hit.name == "Needle current")
                );
                assert!(
                    state
                        .unified_search
                        .results
                        .iter()
                        .all(|hit| hit.name != "Needle old")
                );
                assert!(
                    state
                        .unified_search
                        .results
                        .iter()
                        .all(|hit| !hit.name.contains("Secret episode title"))
                );
                assert_eq!(probe.publications.lock().unwrap().len(), 1);
                assert_eq!(probe.peak_active.load(Ordering::Acquire), 1);
            },
        );
    }
    #[test]
    fn local_search_real_bridge_profile_replacement_bounds_blocked_scans_and_publication() {
        super::super::profile_actions_tests::isolated(
            concat!(
                module_path!(),
                "::local_search_real_bridge_profile_replacement_bounds_blocked_scans_and_publication"
            ),
            |fixture| {
                let bridge = &fixture.bridge;
                let app = bridge.app().unwrap();
                let alice = profiles::current_id();
                let bob = profiles::create("Search Bob").unwrap();
                let entry = |id: &str, name: &str| -> LibraryEntry {
                    serde_json::from_value(serde_json::json!({
                        "id":id, "type_":"series", "name":name, "added_at_secs":1
                    }))
                    .unwrap()
                };
                bridge.shared.lock().unwrap().entries =
                    vec![entry("alice-title", "Final Alice private title")];
                bridge.persist_library();
                profiles::ProfileStore::for_profile(&bob)
                    .write(
                        "library",
                        &serde_json::to_string(&vec![entry(
                            "bob-title",
                            "Final Bob private title",
                        )])
                        .unwrap(),
                    )
                    .unwrap();
                persistence::flush().unwrap();
                let first_session = bridge.sessions.active();
                let first_epoch = first_session.epoch;
                let first_weak = Arc::downgrade(&first_session);
                drop(first_session);
                let reads = Arc::new(Mutex::new(Vec::<(String, u64, String)>::new()));
                let recorded_reads = reads.clone();
                let block_first = AtomicBool::new(true);
                let gate_owner = alice.clone();
                let (entered, waiting) = std::sync::mpsc::sync_channel(1);
                let (release, blocked) = std::sync::mpsc::sync_channel(1);
                let blocked = Mutex::new(blocked);
                let probe = Arc::new(SearchTestProbe {
                    reader: Arc::new(move |context, kind, id| {
                        assert_eq!(kind, "series");
                        recorded_reads.lock().unwrap().push((
                            context.id.clone(),
                            context.epoch,
                            id.into(),
                        ));
                        if context.id == gate_owner
                            && context.epoch == first_epoch
                            && block_first.swap(false, Ordering::AcqRel)
                        {
                            entered.send(()).unwrap();
                            blocked
                                .lock()
                                .unwrap()
                                .recv_timeout(Duration::from_secs(30))
                                .unwrap();
                        }
                        vec![Video {
                            id: format!("{id}:episode"),
                            name: "Final private episode".into(),
                            season: Some(1),
                            episode: Some(1),
                            ..Default::default()
                        }]
                    }),
                    active: AtomicUsize::new(0),
                    peak_active: AtomicUsize::new(0),
                    peak_pending: AtomicUsize::new(0),
                    threads: AtomicUsize::new(0),
                    corpus_scans: AtomicUsize::new(0),
                    availability_scans: AtomicUsize::new(0),
                    publications: Mutex::new(Vec::new()),
                });
                let _scope = SearchTestScope::install(probe.clone());
                let pump = || {
                    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(10));
                    profiles::invoke_account(|| slint::quit_event_loop().unwrap()).unwrap();
                    slint::run_event_loop().unwrap();
                    bridge.sessions.reap();
                    std::thread::sleep(Duration::from_millis(2));
                };
                let submit = |query: &str| {
                    app.set_search_text(query.into());
                    app.set_discover_search_open(true);
                    bridge.pick_search_scope(0);
                    bridge.search_edited(query);
                };
                submit("obsolete Alice");
                let until = std::time::Instant::now() + Duration::from_secs(15);
                loop {
                    match waiting.try_recv() {
                        Ok(()) => break,
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            assert!(
                                std::time::Instant::now() < until,
                                "original Bridge scan never reached inventory gate"
                            );
                            pump();
                        }
                        Err(error) => panic!("inventory gate disconnected: {error}"),
                    }
                }
                let blocked_threads = probe.threads.load(Ordering::Acquire);
                assert_eq!(probe.active.load(Ordering::Acquire), 1);
                bridge.switch_profile(&bob);
                bridge.wait_profile_activation();
                assert!(app.get_profiles_error().is_empty());
                assert_eq!(profiles::current_id(), bob);
                let bob_session = bridge.sessions.active();
                let bob_epoch = bob_session.epoch;
                let bob_weak = Arc::downgrade(&bob_session);
                drop(bob_session);
                for query in ["obsolete Bob one", "obsolete Bob two", "obsolete Bob three"] {
                    submit(query);
                }
                {
                    let pending = LOCAL_SEARCH_WORK.lock().unwrap();
                    let request = pending.latest.as_ref().unwrap();
                    assert_eq!(request.context.id, bob);
                    assert_eq!(request.context.epoch, bob_epoch);
                    assert!(request.context.session.is_some());
                }
                bridge.switch_profile(&alice);
                bridge.wait_profile_activation();
                assert!(app.get_profiles_error().is_empty());
                assert_eq!(profiles::current_id(), alice);
                let final_epoch = bridge.sessions.active().epoch;
                assert_ne!(final_epoch, first_epoch);
                submit("Final");
                {
                    let pending = LOCAL_SEARCH_WORK.lock().unwrap();
                    let request = pending.latest.as_ref().unwrap();
                    assert_eq!(request.context.id, alice);
                    assert_eq!(request.context.epoch, final_epoch);
                    assert_eq!(request.receipt.query, "Final");
                }
                assert_eq!(probe.active.load(Ordering::Acquire), 1);
                assert_eq!(
                    probe.threads.load(Ordering::Acquire),
                    blocked_threads,
                    "real session replacement must not start around a blocked old reader"
                );
                assert_eq!(probe.peak_active.load(Ordering::Acquire), 1);
                assert_eq!(probe.peak_pending.load(Ordering::Acquire), 1);
                assert!(probe.publications.lock().unwrap().is_empty());
                assert!(
                    first_weak.upgrade().is_some(),
                    "the blocked read still owns its original session"
                );
                release.send(()).unwrap();
                let until = std::time::Instant::now() + Duration::from_secs(15);
                loop {
                    pump();
                    let published = !probe.publications.lock().unwrap().is_empty();
                    let idle = !LOCAL_SEARCH_WORK.lock().unwrap().running
                        && probe.active.load(Ordering::Acquire) == 0;
                    if published
                        && idle
                        && first_weak.upgrade().is_none()
                        && bob_weak.upgrade().is_none()
                    {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < until,
                        "final Bridge query must publish and both replaced sessions must release; publications={:?}, old_a={}, old_b={}, idle={idle}",
                        probe.publications.lock().unwrap(),
                        first_weak.upgrade().is_some(),
                        bob_weak.upgrade().is_some()
                    );
                }
                let publications = probe.publications.lock().unwrap();
                assert!(!publications.is_empty());
                for publication in publications.iter() {
                    assert_eq!(publication.profile, alice);
                    assert_eq!(publication.epoch, final_epoch);
                    assert_eq!(publication.query, "Final");
                    assert!(!publication.ids.is_empty());
                    assert!(publication.ids.iter().all(|id| id == "alice-title"));
                }
                drop(publications);
                assert_eq!(probe.peak_active.load(Ordering::Acquire), 1);
                assert_eq!(probe.peak_pending.load(Ordering::Acquire), 1);
                let reads = reads.lock().unwrap();
                assert!(
                    reads
                        .iter()
                        .all(|(owner, _, id)| owner == &alice && id == "alice-title"),
                    "queued Bob corpus must never be scanned: {reads:?}"
                );
                assert!(reads.iter().any(|(_, epoch, _)| *epoch == final_epoch));
                drop(reads);
                let state = bridge.shared.lock().unwrap();
                assert_eq!(state.unified_search.query, "Final");
                assert!(!state.search_previews.is_empty());
                assert!(
                    state
                        .search_previews
                        .iter()
                        .all(|meta| title_id(meta) == "alice-title")
                );
                assert_eq!(
                    app.get_search_results().row_count(),
                    state.search_previews.len()
                );
                assert!(
                    app.get_search_results()
                        .iter()
                        .all(|row| !row.title.contains("Bob"))
                );
                assert!(first_weak.upgrade().is_none());
                assert!(bob_weak.upgrade().is_none());
            },
        );
    }

    #[test]
    fn local_search_saved_facts_notice_same_identity_edits_without_membership_changes() {
        let mut entries: Vec<LibraryEntry> = serde_json::from_value(serde_json::json!([
            {"id":"same-id", "type_":"movie", "name":"First", "year":"2020", "poster_url":"poster-a"},
            {"id":"same-id", "type_":"series", "name":"Other", "year":"2021", "poster_url":"poster-b"}
        ])).unwrap();
        let mut state = State::default();
        state.remember_library(&entries);
        for field in 0..3 {
            let original = entries[0].clone();
            match field {
                0 => entries[0].name = "Corrected title".into(),
                1 => entries[0].year = "2026".into(),
                _ => entries[0].poster_url = "new decoded poster".into(),
            }
            assert!(state.contains_saved("movie", "same-id"));
            assert!(state.contains_saved("series", "same-id"));
            assert!(!state.library_matches(&entries));
            assert!(state.saved_facts_match(&entries[1]));
            state.remember_library(&entries);
            assert!(state.library_matches(&entries));
            entries[0] = original;
            state.remember_library(&entries);
        }
        for _ in 0..100 {
            assert!(state.library_matches(&entries));
        }
    }

    fn work_receipt(query: &str, epoch: u64) -> SearchReceipt {
        SearchReceipt {
            context: profiles::Context {
                id: "coalesced-search-test".into(),
                epoch,
                session: None,
            },
            query: query.into(),
            stamp: CorpusStamp {
                semantic: 0,
                clock: Arc::default(),
                downloads: None,
                download_revision: 0,
                metadata: 1,
                reveals: 1,
                local: 0,
                instance: 1,
            },
        }
    }

    #[test]
    fn local_search_work_coalesces_and_reuses_only_exact_receipts() {
        let mut work = SearchWork::default();
        assert_eq!(work.submit(work_receipt("query-0", 1)), Some((1, true)));
        let first = work.take_next().unwrap();
        for index in 1..20 {
            assert_eq!(
                work.submit(work_receipt(&format!("query-{index}"), 1)),
                Some((index + 1, false))
            );
            assert!(work.latest.is_some());
        }
        assert!(!work.current(&first));
        let latest = work.take_next().unwrap();
        assert_eq!(latest.receipt.query, "query-19");
        assert!(work.current(&latest));
        assert!(work.submit(latest.receipt.clone()).is_none());
        assert!(work.take_next().is_none());
        assert!(!work.running);
        assert!(
            work.submit(latest.receipt.clone()).is_none(),
            "settled start reuses the completed query"
        );
        let mut changed = latest.receipt.clone();
        changed.stamp.local += 1;
        assert_eq!(work.submit(changed), Some((21, true)));
        assert!(!work.current(&latest));
    }

    #[test]
    fn local_search_work_exit_submit_short_query_and_profile_races_are_lossless() {
        let mut work = SearchWork::default();
        work.submit(work_receipt("first", 1));
        let first = work.take_next().unwrap();
        assert_eq!(
            work.submit(work_receipt("before-exit", 1)),
            Some((2, false))
        );
        let next = work.take_next().unwrap();
        assert_eq!(next.receipt.query, "before-exit");
        assert!(!work.current(&first));
        assert!(work.take_next().is_none());
        assert_eq!(work.submit(work_receipt("after-exit", 1)), Some((3, true)));
        let old_profile = work.take_next().unwrap();
        assert_eq!(work.submit(work_receipt("after-exit", 2)), Some((4, false)));
        assert!(
            !work.current(&old_profile),
            "old activation cannot publish into the new one"
        );
        assert_eq!(work.take_next().unwrap().receipt.context.epoch, 2);
        assert_eq!(work.submit(work_receipt("x", 2)), Some((5, false)));
        assert!(work.latest.is_none(), "short query cancels pending work");
        assert!(work.take_next().is_none());
        assert!(!work.running);
        assert_eq!(work.submit(work_receipt("retry", 2)), Some((6, true)));
        let failed = work.take_next().unwrap();
        work.abandon(&failed);
        assert!(work.take_next().is_none());
        assert_eq!(
            work.submit(failed.receipt),
            Some((7, true)),
            "failed dispatch can retry"
        );
    }

    #[test]
    fn local_search_inventory_lookup_checks_cancellation_every_64_episodes() {
        let videos: Vec<_> = (0..200)
            .map(|index| Video {
                id: index.to_string(),
                ..Default::default()
            })
            .collect();
        let checks = std::cell::Cell::new(0);
        let current = || {
            checks.set(checks.get() + 1);
            checks.get() == 1
        };
        assert!(find_current_video(&videos, "64", &current).is_none());
        assert_eq!(checks.get(), 2);
        checks.set(0);
        assert_eq!(
            find_current_video(&videos, "63", &current).unwrap().id,
            "63"
        );
        assert_eq!(checks.get(), 1);
    }

    #[test]
    fn local_search_state_replacement_keeps_one_blocked_scanner_and_latest_owner() {
        const CHILD: &str = "NOVA_TEST_SEARCH_RESET_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::unified_search::tests::local_search_state_replacement_keeps_one_blocked_scanner_and_latest_owner", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let original_state = State::default();
        let work = original_state.work.clone();
        let mut receipt = work_receipt("same query", 1);
        receipt.stamp.instance = original_state.instance;
        assert_eq!(work.lock().unwrap().submit(receipt), Some((1, true)));
        let original = work.lock().unwrap().take_next().unwrap();
        let (release, blocked) = std::sync::mpsc::channel();
        let (finished, result) = std::sync::mpsc::channel();
        let worker_work = work.clone();
        let worker = std::thread::spawn(move || {
            // Stand in for one non-interruptible inventory read. Resetting
            // sessions must not start another scanner while this slot is held.
            blocked.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(!worker_work.lock().unwrap().current(&original));
            let latest = worker_work.lock().unwrap().take_next().unwrap();
            assert!(worker_work.lock().unwrap().current(&latest));
            finished
                .send((latest.receipt.context.epoch, latest.receipt.stamp.instance))
                .unwrap();
            assert!(worker_work.lock().unwrap().take_next().is_none());
        });
        let mut final_instance = 0;
        for epoch in 2..=101 {
            // PreparedProfile and SessionDirectory replacement create exactly
            // this fresh State through Shared::default().
            let replacement = State::default();
            assert!(Arc::ptr_eq(&work, &replacement.work));
            assert_ne!(original_state.instance, replacement.instance);
            final_instance = replacement.instance;
            let mut receipt = work_receipt("same query", epoch);
            receipt.stamp.instance = replacement.instance;
            assert_eq!(
                replacement.work.lock().unwrap().submit(receipt),
                Some((epoch, false))
            );
            assert!(replacement.work.lock().unwrap().latest.is_some());
        }
        release.send(()).unwrap();
        assert_eq!(
            result.recv_timeout(Duration::from_secs(10)).unwrap(),
            (101, final_instance)
        );
        worker.join().unwrap();
        assert!(!work.lock().unwrap().running);
    }

    #[test]
    fn local_search_cancelled_inventory_stops_before_next_read_and_within_64_rows() {
        const CHILD: &str = "NOVA_TEST_SEARCH_CANCELLATION_CHILD";
        let Ok(root) = std::env::var(CHILD) else {
            let root = std::env::temp_dir()
                .join(format!("nova-search-cancellation-{}", std::process::id()));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::unified_search::tests::local_search_cancelled_inventory_stops_before_next_read_and_within_64_rows", "--nocapture"])
                .env(CHILD, &root).output().unwrap();
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
        let known: Vec<_> = (0..3)
            .map(|index| MetaPreview {
                id: format!("title-{index}"),
                type_: "series".into(),
                name: "Title".into(),
                ..Default::default()
            })
            .collect();
        let spoilers = SpoilerSnapshot {
            protected: false,
            watched: HashSet::new(),
            revealed: HashSet::new(),
        };
        let reads = std::cell::Cell::new(0);
        let episode_checks = std::cell::Cell::new(0);
        let current = || {
            if reads.get() == 0 {
                true
            } else {
                episode_checks.set(episode_checks.get() + 1);
                episode_checks.get() <= 1
            }
        };
        let result = collect_results_with_reader(
            "no match",
            &[],
            &known,
            Vec::new(),
            &spoilers,
            current,
            |_, _| {
                reads.set(reads.get() + 1);
                (0..200)
                    .map(|index| Video {
                        id: format!("episode-{index}"),
                        ..Default::default()
                    })
                    .collect()
            },
        );
        assert!(result.is_none());
        assert_eq!(
            reads.get(),
            1,
            "no second inventory may be read after cancellation"
        );
        assert_eq!(
            episode_checks.get(),
            2,
            "checks at episode0 and64 bound stale iteration"
        );

        let directory = profile_session::SessionDirectory::new();
        let session = directory.active();
        let weak = Arc::downgrade(&session);
        let mut receipt = work_receipt("retained receipt", session.epoch);
        receipt.context = session.context();
        let mut work = SearchWork::default();
        work.submit(receipt);
        assert!(work.wanted.as_ref().unwrap().context.session.is_none());
        drop(work.take_next());
        drop(session);
        drop(directory);
        assert!(
            weak.upgrade().is_none(),
            "a cached receipt must not retain an activation"
        );
    }

    fn preview(id: &str, kind: &str, name: &str) -> MetaPreview {
        MetaPreview {
            id: id.into(),
            type_: kind.into(),
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn identity_dedup_requires_global_proof_and_catalogs_cannot_forge_local_actions() {
        assert_ne!(
            episode_result_id("series", "show:1", "2:3"),
            episode_result_id("series", "show", "1:2:3")
        );
        let mut a = preview("opaque", "movie", "Same title");
        let mut b = a.clone();
        tag_catalog(&mut a, "https://one.example/manifest.json");
        b.extra
            .insert(ACTION.into(), serde_json::to_value(Action::Title).unwrap());
        b.extra
            .insert(LABEL.into(), "Available offline · Play".into());
        tag_catalog(&mut b, "https://two.example/manifest.json");
        assert_ne!(result_key(&a), result_key(&b));
        assert_eq!(
            restored_focus(&[a.clone(), b.clone()], &[b.clone(), a.clone()], 0),
            Some(1)
        );
        assert!(action(&b).is_none());
        assert!(!b.extra.contains_key(LABEL));
        a.id = "tt1234567".into();
        b.id = a.id.clone();
        assert_eq!(result_key(&a), result_key(&b));
        b.type_ = "series".into();
        assert_ne!(result_key(&a), result_key(&b));

        let saved = document(
            a.clone(),
            a.id.clone(),
            a.name.clone(),
            "Saved",
            String::new(),
            Action::Title,
        );
        let episode = document(
            a.clone(),
            "local:episode:one".into(),
            "S1 E2".into(),
            "Episode",
            a.name.clone(),
            Action::Episode {
                title: a.id.clone(),
                video: Video {
                    id: "exact:episode".into(),
                    ..Default::default()
                },
            },
        );
        let mut state = Shared::default();
        state.unified_search.results = vec![saved.clone(), episode.clone()].into();
        assert_eq!(
            state.unified_search.scope, 1,
            "Catalogs is the initial scope"
        );
        assert_eq!(merge(&state, vec![a.clone()]).len(), 1);
        state.unified_search.scope = 0;
        assert_eq!(
            merge(&state, vec![a.clone()]).len(),
            2,
            "title dedups while episode stays separate"
        );
        state.unified_search.scope = 1;
        state.entries = serde_json::from_value(serde_json::json!([
            {"id":"tt1234567","type_":"movie","name":"Same title"},
            {"id":"opaque","type_":"movie","name":"Same title"}
        ]))
        .unwrap();
        let unsaved = preview("tt9876543", "movie", "Same title");
        let mut opaque_one = preview("opaque", "movie", "Same title");
        let mut opaque_two = opaque_one.clone();
        opaque_one.extra.insert(LABEL.into(), "Saved".into());
        tag_catalog(&mut opaque_one, "https://one.example/manifest.json");
        tag_catalog(&mut opaque_two, "https://two.example/manifest.json");
        assert!(
            !opaque_one.extra.contains_key(LABEL),
            "addon cannot forge Saved"
        );
        let remote = vec![a.clone(), b.clone(), unsaved, opaque_one, opaque_two];
        let catalog = merge(&state, remote.clone());
        assert_eq!(
            catalog.len(),
            remote.len(),
            "Catalogs retains only its original catalog hits"
        );
        assert_eq!(card_text(&catalog[0]).0, text::tr("Saved"));
        assert!(
            catalog[1..].iter().all(|meta| card_text(meta).0.is_empty()),
            "wrong media type, same name and ambiguous opaque owners do not prove Saved"
        );
        assert!(
            catalog.iter().all(|meta| action(meta).is_none()),
            "catalog activation keeps its original action"
        );
        assert_eq!(
            catalog.iter().map(result_key).collect::<Vec<_>>(),
            remote.iter().map(result_key).collect::<Vec<_>>(),
            "saved annotation does not reorder or rewrite catalog identities"
        );
        assert_eq!(card_text(&catalog[0]).1, card_text(&remote[0]).1);
        assert!(
            !remote[0].extra.contains_key(LABEL),
            "provider previews are unchanged"
        );
        state.entries.clear();
        assert!(
            merge(&state, remote)
                .iter()
                .all(|meta| card_text(meta).0.is_empty()),
            "removing a saved title clears its presentation marker"
        );
        state.unified_search.scope = 3;
        assert_eq!(merge(&state, vec![a])[0].id, episode.id);
        state.unified_search.scope = 5;
        assert!(merge(&state, vec![]).is_empty());
        state.unified_search.clear_scope();
        assert_eq!(
            state.unified_search.scope, 1,
            "closing search restores Catalogs"
        );
        assert!(
            card_text(&b).0.is_empty(),
            "catalogs have no generic action badge"
        );
        assert_eq!(card_text(&saved).0, text::tr("Saved"));
        assert_eq!(card_text(&episode).0, text::tr("Episode"));
    }

    #[test]
    fn bounded_scopes_preserve_saved_downloads_and_deterministic_order() {
        let parent = preview("tt9999999", "movie", "Z saved film");
        let saved = document(
            parent.clone(),
            parent.id.clone(),
            parent.name.clone(),
            "Saved",
            String::new(),
            Action::Title,
        );
        let offline = document(
            parent.clone(),
            parent.id.clone(),
            parent.name.clone(),
            "Available offline · Play",
            String::new(),
            Action::Download {
                job: "saved-job".into(),
                title: parent.id.clone(),
            },
        );
        let mut candidates = Vec::new();
        for number in 0..LIMIT + 10 {
            for (prefix, action) in [
                (
                    "episode",
                    Action::Episode {
                        title: "series".into(),
                        video: Video {
                            id: format!("episode:{number}"),
                            ..Default::default()
                        },
                    },
                ),
                (
                    "download",
                    Action::Download {
                        job: format!("job:{number}"),
                        title: format!("title:{number}"),
                    },
                ),
                (
                    "bookmark",
                    Action::Bookmark {
                        scene: format!("scene:{number}"),
                        title: "series".into(),
                    },
                ),
            ] {
                let source_title = match &action {
                    Action::Episode { title, .. }
                    | Action::Download { title, .. }
                    | Action::Bookmark { title, .. } => title,
                    Action::Title => unreachable!(),
                };
                let source_kind = if matches!(action, Action::Download { .. }) {
                    "movie"
                } else {
                    "series"
                };
                candidates.push(document(
                    preview(source_title, source_kind, "Unrelated title"),
                    format!("local:{prefix}:{number}"),
                    format!("A {prefix} {number:03}"),
                    "",
                    String::new(),
                    action,
                ));
            }
        }
        let collect = |candidates: &[MetaPreview]| {
            let mut results = ScopedResults::default();
            results.insert(saved.clone(), true);
            for candidate in candidates {
                results.insert(candidate.clone(), false);
            }
            results.insert(offline.clone(), true);
            assert!(results.buckets.iter().all(|bucket| bucket.len() <= LIMIT));
            results.finish()
        };
        let published = collect(&candidates);
        assert!(published.len() <= LIMIT * 5);
        candidates.reverse();
        assert_eq!(
            published.iter().map(result_key).collect::<Vec<_>>(),
            collect(&candidates)
                .iter()
                .map(result_key)
                .collect::<Vec<_>>()
        );
        let mut state = Shared::default();
        state.entries = serde_json::from_value(serde_json::json!([
            {"id":"tt9999999","type_":"movie","name":"Z saved film"}
        ]))
        .unwrap();
        state.unified_search.results = published.into();
        for scope in [0, 3, 4, 5] {
            state.unified_search.scope = scope;
            assert_eq!(merge(&state, vec![]).len(), LIMIT);
        }
        state.unified_search.scope = 2;
        let library = merge(&state, vec![]);
        assert_eq!(
            library.len(),
            1,
            "unrelated downloads cannot exhaust Library's budget"
        );
        assert!(
            matches!(action(&library[0]), Some(Action::Download { job, .. }) if job == "saved-job")
        );
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn accepted_library_edits_refresh_retained_search_without_resetting_catalogs() {
        const CHILD: &str = "NOVA_TEST_SEARCH_MEMBERSHIP_CHILD";
        let Ok(root) = std::env::var(CHILD) else {
            let root =
                std::env::temp_dir().join(format!("nova-search-membership-{}", std::process::id()));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::unified_search::tests::accepted_library_edits_refresh_retained_search_without_resetting_catalogs", "--nocapture"])
                .env(CHILD, &root).output().unwrap();
            let _ = std::fs::remove_dir_all(root);
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
        app.global::<crate::Anim>().set_enabled(false);
        app.set_show_home(false);
        app.set_discover_search_open(true);
        app.set_discover_search_filters_open(true);
        app.set_search_text("Signal".into());
        let player = crate::player::Player::setup(&app);
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx::closed(),
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        let mut first = preview("tt1234567", "movie", "Signal");
        let mut second = preview("tt7654321", "movie", "Signal");
        let source = "https://catalog.invalid/manifest.json";
        tag_catalog(&mut first, source);
        tag_catalog(&mut second, source);
        {
            let mut state = bridge.shared.lock().unwrap();
            state.search = "Signal".into();
            state.search_generation = 7;
            state.search_filters.genre = "Drama".into();
            state.search_targets = vec![SearchTarget {
                addon_url: source.into(),
                type_: "movie".into(),
                catalog_id: "demo".into(),
                genre_options: vec!["Drama".into()],
                genre: "Drama".into(),
                supports_skip: true,
                next_skip: 100,
                exhausted: true,
                page_fingerprints: HashSet::new(),
                results: vec![first.clone(), second.clone()].into(),
            }];
        }
        bridge.refresh_local_search("Signal");
        settle_semantic_search(&bridge);
        app.set_kb_active(true);
        app.set_discover_kb_zone(3);
        app.set_discover_kb_idx(0);
        let original_keys = bridge
            .shared
            .lock()
            .unwrap()
            .search_previews
            .iter()
            .map(result_key)
            .collect::<Vec<_>>();
        let badge = |id: &str| {
            app.get_search_results()
                .iter()
                .find(|card| card.id == id)
                .unwrap()
                .badge
                .to_string()
        };
        let open = |meta: &MetaPreview| {
            bridge.shared.lock().unwrap().modal_item = Some(ModalItem {
                details: Default::default(),
                open_token: Arc::new(()),
                pending_watch_now: None,
                episodes_loading: false,
                id: meta.id.clone(),
                type_: meta.type_.clone(),
                request_id: meta.id.clone(),
                videos: Vec::new(),
                season_backdrops: HashMap::new(),
                seasons: Vec::new(),
                season_index: 0,
                episode_page: 0,
                name: meta.name.clone(),
                year: String::new(),
                poster_url: String::new(),
                background_url: String::new(),
                logo_url: String::new(),
                description: String::new(),
                genres: Vec::new(),
            });
            app.set_modal_visible(true);
        };
        let back = || {
            app.set_modal_visible(false);
            bridge.modal_closed();
        };
        assert!(badge(&first.id).is_empty());
        open(&first);
        bridge.toggle_current_in_library();
        back();
        assert_eq!(badge(&first.id), text::tr("Saved"));
        assert!(badge(&second.id).is_empty());
        let check_catalogs = || {
            let state = bridge.shared.lock().unwrap();
            assert_eq!(
                state.search_generation, 7,
                "membership edits do not submit another search"
            );
            assert_eq!(state.search_pending_requests, 0);
            assert_eq!(state.search_targets[0].next_skip, 100);
            assert_eq!(state.search_filters.genre, "Drama");
            assert_eq!(
                state
                    .search_previews
                    .iter()
                    .map(result_key)
                    .collect::<Vec<_>>(),
                original_keys
            );
            assert!(
                state
                    .search_previews
                    .iter()
                    .all(|meta| action(meta).is_none())
            );
            assert_eq!(app.get_search_scope(), 1);
            assert_eq!(app.get_search_text(), "Signal");
            assert_eq!(app.get_discover_kb_zone(), 3);
            assert_eq!(app.get_discover_kb_idx(), 0);
        };
        check_catalogs();
        // Merely opening/reviewing the duplicate warning is not an accepted edit.
        open(&second);
        let before = bridge.shared.lock().unwrap().unified_search.revision;
        bridge.toggle_current_in_library();
        assert!(app.get_library_duplicates_open());
        assert_eq!(
            bridge.shared.lock().unwrap().unified_search.revision,
            before
        );
        bridge.library_duplicate_action(0);
        assert_eq!(
            bridge.shared.lock().unwrap().unified_search.revision,
            before
        );
        bridge.library_duplicate_action(-3);
        back();
        assert!(badge(&first.id).is_empty());
        assert_eq!(badge(&second.id), text::tr("Saved"));
        check_catalogs();
        bridge.remove_library_at(0);
        assert!(badge(&second.id).is_empty());
        check_catalogs();
        // External/admitted projections publish through the same boundary,
        // including the prepared-card fast path while Library is offscreen.
        let external: LibraryEntry = serde_json::from_value(
            serde_json::json!({"id":first.id,"type_":"movie","name":"Signal"}),
        )
        .unwrap();
        bridge.shared.lock().unwrap().entries = vec![external];
        bridge.publish_progress_cards(Vec::new());
        assert_eq!(badge(&first.id), text::tr("Saved"));
        let before = bridge.shared.lock().unwrap().unified_search.revision;
        bridge.publish_progress_cards(Vec::new());
        assert_eq!(
            bridge.shared.lock().unwrap().unified_search.revision,
            before,
            "unchanged publications do not start another worker"
        );
        bridge.shared.lock().unwrap().entries.clear();
        bridge.publish_progress_cards(Vec::new());
        assert!(badge(&first.id).is_empty());
        check_catalogs();
        // All must retract its cached local Title too, allowing the original
        // catalog action to return after that title is removed from Library.
        bridge.pick_search_scope(0);
        open(&first);
        bridge.toggle_current_in_library();
        assert!(
            bridge
                .shared
                .lock()
                .unwrap()
                .search_previews
                .iter()
                .any(|meta| meta.id == first.id && matches!(action(meta), Some(Action::Title)))
        );
        bridge.toggle_current_in_library();
        back();
        let state = bridge.shared.lock().unwrap();
        assert_eq!(state.unified_search.scope, 0);
        assert!(
            state
                .unified_search
                .results
                .iter()
                .all(|meta| !matches!(action(meta), Some(Action::Title)))
        );
        assert!(
            state
                .search_previews
                .iter()
                .all(|meta| action(meta).is_none())
        );
        assert!(badge(&first.id).is_empty());
    }

    #[test]
    fn local_sources_are_profile_owned_spoiler_safe_and_require_a_usable_download() {
        const CHILD: &str = "NOVA_TEST_UNIFIED_SEARCH_CHILD";
        let Ok(root) = std::env::var(CHILD) else {
            let root =
                std::env::temp_dir().join(format!("nova-unified-search-{}", std::process::id()));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::unified_search::tests::local_sources_are_profile_owned_spoiler_safe_and_require_a_usable_download", "--nocapture"])
                .env(CHILD, &root).output().unwrap();
            let _ = std::fs::remove_dir_all(root);
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
        let shared = profile_session::SessionState(profile_session::SessionDirectory::new());
        shared.lock().unwrap().cache_settings.spoiler_protection = true;
        let alice = profiles::ProfileStore::for_profile("search-alice");
        let bob = profiles::ProfileStore::for_profile("search-bob");
        let videos = vec![Video {
            id: "episode:exact".into(),
            name: "Hidden finale".into(),
            season: Some(1),
            episode: Some(2),
            ..Default::default()
        }];
        alice
            .write(
                &episodes_key("series", "tt7654321"),
                &serde_json::to_string(&videos).unwrap(),
            )
            .unwrap();
        bob.write(
            &episodes_key("series", "other-title"),
            r#"[{"id":"private","name":"Only Bob"}]"#,
        )
        .unwrap();
        let identity = nova_playback_api::MediaIdentity {
            media_type: "series".into(),
            title_id: "tt7654321".into(),
            episode_id: "episode:exact".into(),
            title: "Local series".into(),
            episode: "Hidden finale".into(),
            ..Default::default()
        };
        let bookmark = bookmarks::new_bookmark(123.0, "Private scene note", &identity, "");
        let key = bookmarks::identity_key(&identity);
        let scene = bookmarks::scene_id(&key, &bookmark);
        let mut legacy = bookmark.clone();
        legacy.note = "Legacy private marker".into();
        legacy.metadata = None;
        let legacy_key = serde_json::to_string(&("movie", "tt1111111", "tt1111111")).unwrap();
        alice
            .write(
                bookmarks::BOOKMARKS_KEY,
                &serde_json::to_string(&HashMap::from([
                    (key, vec![bookmark]),
                    (legacy_key, vec![legacy]),
                ]))
                .unwrap(),
            )
            .unwrap();
        let path = root.join("completed.mp4");
        std::fs::write(&path, b"usable local file").unwrap();
        let good = DownloadJob {
            id: "good".into(),
            media_type: "series".into(),
            media_id: "tt7654321".into(),
            request_id: "episode:exact".into(),
            title: "Local series".into(),
            phase: crate::download::DownloadPhase::Completed,
            artifact_path: Some(path),
            ..Default::default()
        };
        let mut missing = good.clone();
        missing.id = "missing".into();
        missing.artifact_path = Some(root.join("missing.mp4"));
        let mut paused = good.clone();
        paused.id = "paused".into();
        paused.phase = crate::download::DownloadPhase::Paused;
        let entries: Vec<LibraryEntry> = serde_json::from_value(
            serde_json::json!([{"id":"tt7654321","type_":"series","name":"Local series"}]),
        )
        .unwrap();
        alice.run(|| {
            let collect = |query: &str| collect_results(query, &entries, &[], vec![good.clone(), missing.clone(), paused.clone()], &shared);
            assert!(collect("Hidden finale").is_empty(), "masked episode names must not be searchable");
            assert!(collect("Only Bob").is_empty(), "another profile's cache must stay private");
            let legacy = collect("Legacy private marker");
            assert_eq!(legacy.len(), 1, "legacy note without cached parent metadata stays searchable");
            assert!(matches!(action(&legacy[0]), Some(Action::Bookmark { title, .. }) if title == "tt1111111"));
            let notes = collect("Private scene");
            assert_eq!(notes.len(), 1);
            assert_eq!(card_text(&notes[0]).0, text::tr("Bookmark"));
            assert!(matches!(action(&notes[0]), Some(Action::Bookmark { scene: found, .. }) if found == scene));
            let numbered = collect("S1 E2");
            assert_eq!(numbered.iter().filter(|meta| matches!(action(meta), Some(Action::Episode { .. }))).count(), 1);
            assert_eq!(numbered.iter().filter(|meta| matches!(action(meta), Some(Action::Download { .. }))).count(), 1);
            assert!(numbered.iter().all(|meta| !meta.name.contains("Hidden finale")));
            assert!(!usable(&missing, &alice));
            assert!(!usable(&paused, &alice));
            assert!(usable(&good, &alice));
            // A missing Video cannot make an opaque provider slug searchable
            // while protection is enabled. Keep the exact IDs in actions only.
            let mut uncached = good.clone();
            uncached.id = "uncached-job".into();
            uncached.request_id = "provider:secret-finale-slug".into();
            let uncached_identity = nova_playback_api::MediaIdentity {
                episode_id: uncached.request_id.clone(),
                ..identity.clone()
            };
            let uncached_bookmark = bookmarks::new_bookmark(234.0, "Z saved note", &uncached_identity, "");
            let uncached_key = bookmarks::identity_key(&uncached_identity);
            let uncached_scene = bookmarks::scene_id(&uncached_key, &uncached_bookmark);
            let mut saved_bookmarks = bookmarks::load_bookmarks().unwrap();
            saved_bookmarks.insert(uncached_key, vec![uncached_bookmark]);
            alice.write(bookmarks::BOOKMARKS_KEY, &serde_json::to_string(&saved_bookmarks).unwrap()).unwrap();
            let missing_metadata = |query: &str| collect_results(query, &entries, &[], vec![uncached.clone()], &shared);
            assert!(missing_metadata("secret-finale-slug").is_empty());
            let masked = missing_metadata("Local series");
            assert!(masked.iter().all(|meta| !meta.name.contains("secret-finale-slug") && !card_text(meta).1.contains("secret-finale-slug")));
            assert!(masked.iter().any(|meta| matches!(action(meta), Some(Action::Download { job, title }) if job == "uncached-job" && title == identity.title_id)));
            assert!(masked.iter().any(|meta| matches!(action(meta), Some(Action::Bookmark { scene, .. }) if scene == uncached_scene)));
            let mut numbered_uncached = uncached.clone();
            numbered_uncached.request_id = "provider:secret-finale-slug:3:7".into();
            let numbered = collect_results("S3 E7", &entries, &[], vec![numbered_uncached], &shared);
            assert_eq!(numbered.len(), 1);
            assert!(numbered[0].name.ends_with("S3 E7"));
            spoiler::reveal("series", &identity.title_id, &uncached.request_id);
            let revealed = missing_metadata("secret-finale-slug");
            assert_eq!(revealed.len(), 2, "transient reveals still apply to uncached actions");

            // Publish a worker result with more than the shared old cap, then
            // switch every dedicated scope without recomputing its documents.
            let long_entries: Vec<LibraryEntry> = serde_json::from_value(serde_json::json!([
                {"id":"long-show","type_":"series","name":"The Office"}
            ])).unwrap();
            let long_videos: Vec<_> = (1..=LIMIT + 10).map(|episode| Video {
                id: format!("long-show:1:{episode}"),
                season: Some(1),
                episode: Some(episode as u32),
                ..Default::default()
            }).collect();
            alice.write(&episodes_key("series", "long-show"), &serde_json::to_string(&long_videos).unwrap()).unwrap();
            let long_identity = nova_playback_api::MediaIdentity {
                title_id: "long-show".into(),
                episode_id: long_videos[0].id.clone(),
                title: "The Office".into(),
                ..identity.clone()
            };
            let long_bookmark = bookmarks::new_bookmark(345.0, "Z office note", &long_identity, "");
            saved_bookmarks.insert(bookmarks::identity_key(&long_identity), vec![long_bookmark]);
            alice.write(bookmarks::BOOKMARKS_KEY, &serde_json::to_string(&saved_bookmarks).unwrap()).unwrap();
            let long_job = DownloadJob {
                id: "long-job".into(), media_id: "long-show".into(),
                request_id: long_videos[0].id.clone(), title: "The Office".into(),
                ..good.clone()
            };
            let mut state = Shared::default();
            state.entries = long_entries.clone();
            state.unified_search.scope = 2;
            state.unified_search.results = saved_results("Office", &long_entries).into();
            assert_eq!(merge(&state, vec![]).len(), 1, "immediate Library result");
            let published = collect_results("Office", &long_entries, &[], vec![long_job.clone()], &shared);
            assert!(published.len() <= LIMIT * 5);
            state.unified_search.results = published.clone().into();
            for (scope, count) in [(0, LIMIT), (2, 2), (3, LIMIT), (4, 1), (5, 1)] {
                state.unified_search.scope = scope;
                let shown = merge(&state, vec![]);
                assert_eq!(shown.len(), count, "scope {scope} survives worker publication");
            }
            let mut reversed = long_videos;
            reversed.reverse();
            alice.write(&episodes_key("series", "long-show"), &serde_json::to_string(&reversed).unwrap()).unwrap();
            let reordered = collect_results("Office", &long_entries, &[], vec![long_job], &shared);
            assert_eq!(published.iter().map(result_key).collect::<Vec<_>>(), reordered.iter().map(result_key).collect::<Vec<_>>());
            let policy = profile_policy::ProfilePolicy { restricted: true, ..Default::default() };
            alice.write(profile_policy::KEY, &serde_json::to_string(&policy).unwrap()).unwrap();
            assert!(collect("Local").is_empty(), "restricted profiles cannot search disallowed titles");
            assert!(!usable(&good, &alice));
        });
    }
}

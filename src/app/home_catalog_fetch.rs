//! Bounded raw Home catalog backfill. Workers use only captured inputs and
//! local evidence; UI continuations validate ownership before issuing more I/O.
use super::*;
use nova_config::{CatalogGroupingKey, CatalogGroupingMode, CatalogSurface};
use nova_providers::catalog_grouping::{self as grouping, OwnedCatalogEntry};

const PAGE_LIMIT: usize = 3;
const RAW_LIMIT: usize = 120;

#[derive(Clone, Copy)]
pub(super) enum Scope {
    Showcase(u64),
    Rows(u64),
}

impl Scope {
    fn current(self, bridge: &Bridge) -> bool {
        match self {
            Self::Showcase(generation) => {
                bridge.home_showcase_gen.load(Ordering::Relaxed) == generation
            }
            Self::Rows(generation) => {
                bridge.home_catalog_rows_gen.load(Ordering::Relaxed) == generation
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    Ready,
    Original,
    Enough,
    Unsupported,
    Empty,
    Repeated,
    Failed,
    Budget,
    Changed,
}

struct Cursor {
    raw: Vec<MetaPreview>,
    seen: HashSet<(String, String)>,
    fingerprints: HashSet<String>,
    next_skip: usize,
    inspected: usize,
    pages: usize,
    successful: bool,
    stop: Stop,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            raw: vec![],
            seen: HashSet::new(),
            fingerprints: HashSet::new(),
            next_skip: 0,
            inspected: 0,
            pages: 0,
            successful: false,
            stop: Stop::Ready,
        }
    }
}

impl Cursor {
    fn record(
        &mut self,
        source: &HomeCatalogSource,
        page: Option<Vec<MetaPreview>>,
        mode: CatalogGroupingMode,
    ) {
        self.pages += 1;
        let Some(mut page) = page else {
            self.stop = Stop::Failed;
            return;
        };
        self.successful = true;
        // Pagination belongs to the server response, before exact-ID dedup,
        // projection, or the bounded number of retained raw candidates.
        self.next_skip = self.next_skip.saturating_add(page.len());
        if page.is_empty() {
            self.stop = Stop::Empty;
            return;
        }
        let mut identities: Vec<_> = page
            .iter()
            .map(|preview| (&preview.type_, &preview.id))
            .collect();
        identities.sort();
        identities.dedup();
        if !self
            .fingerprints
            .insert(grouping::projection_fingerprint(&identities))
        {
            self.stop = Stop::Repeated;
            return;
        }
        if mode != CatalogGroupingMode::Original {
            page.truncate(RAW_LIMIT.saturating_sub(self.inspected));
        }
        self.inspected += page.len();
        for mut preview in page {
            // An addon cannot claim another installation's ownership through
            // extension JSON. Native IDs, artwork and order are preserved.
            preview
                .extra
                .insert("novaSourceUrl".into(), serde_json::json!(source.addon_url));
            if self
                .seen
                .insert((preview.type_.clone(), preview.id.clone()))
            {
                self.raw.push(preview);
            }
        }
    }

    fn decide(
        &mut self,
        mode: CatalogGroupingMode,
        supports_skip: bool,
        desired: usize,
        visible: usize,
    ) -> bool {
        if self.stop != Stop::Ready {
            return false;
        }
        self.stop = if mode == CatalogGroupingMode::Original {
            Stop::Original
        } else if visible >= desired {
            Stop::Enough
        } else if !supports_skip {
            Stop::Unsupported
        } else if self.pages >= PAGE_LIMIT || self.inspected >= RAW_LIMIT {
            Stop::Budget
        } else {
            return true;
        };
        false
    }

    fn finish(self) -> Option<Vec<MetaPreview>> {
        self.successful.then_some(self.raw)
    }
}

fn grouping_key(source: &HomeCatalogSource) -> CatalogGroupingKey {
    CatalogGroupingKey {
        addon_url: source.addon_url.clone(),
        type_: source.type_.clone(),
        catalog_id: source.catalog_id.clone(),
        surface: CatalogSurface::Home,
    }
}

fn visible_count(
    source: &HomeCatalogSource,
    raw: &[MetaPreview],
    mode: CatalogGroupingMode,
    revision: u64,
) -> usize {
    if mode == CatalogGroupingMode::Original {
        return raw.len();
    }
    let entries = raw
        .iter()
        .map(|preview| {
            let mut entry = OwnedCatalogEntry::new(&source.addon_url, preview.clone());
            // A malformed/conflicting explicit parent declaration cannot be hidden
            // by older cache proof, even if that proof once described this source.
            if !preview.extra.contains_key("novaCatalogGrouping") || entry.evidence.is_some() {
                let cached = grouping::cached_evidence(&entry.source, revision).flatten();
                entry.evidence = grouping::merge_grouping_evidence(entry.evidence, cached);
            }
            entry
        })
        .collect::<Vec<_>>();
    grouping::project_catalog(&entries, mode).len()
}

fn next_url(source: &HomeCatalogSource, skip: usize) -> Option<String> {
    let addon = Addon::new(&source.addon_url).ok()?;
    let skip = skip.to_string();
    let mut extra = vec![("skip", skip.as_str())];
    if !source.genre.is_empty() {
        extra.push(("genre", source.genre.as_str()));
    }
    Some(addon.catalog_url(&source.type_, &source.catalog_id, &extra))
}

struct Job {
    bridge: Bridge,
    profile: profiles::Context,
    scope: Scope,
    source: HomeCatalogSource,
    owner_generation: Option<u64>,
    mode: CatalogGroupingMode,
    revision: u64,
    supports_skip: bool,
    desired: usize,
    index: usize,
    cursor: Cursor,
}

impl Job {
    /// Runs only on the UI thread, immediately before a continuation or delivery.
    fn current(&self) -> bool {
        self.scope.current(&self.bridge)
            && profiles::is_current(&self.profile)
            && self
                .bridge
                .shared
                .lock()
                .unwrap()
                .installed
                .iter()
                .any(|addon| {
                    addon.url == self.source.addon_url
                        && addon.enabled
                        && addon.available
                        && Some(addon.generation) == self.owner_generation
                })
    }

    fn allow_more(&self) -> bool {
        if self.revision != nova_providers::metadata_revision() {
            return false;
        }
        let state = self.bridge.shared.lock().unwrap();
        state
            .cache_settings
            .catalog_grouping_mode(&grouping_key(&self.source))
            == self.mode
            && state.installed.iter().any(|addon| {
                addon.url == self.source.addon_url
                    && addon.enabled
                    && addon.available
                    && Some(addon.generation) == self.owner_generation
            })
    }
}

#[derive(Clone)]
struct Completion<F> {
    results: Arc<Mutex<Vec<(usize, home::HomeShowcaseBatch)>>>,
    remaining: Arc<AtomicUsize>,
    finish: F,
}

impl<F: FnOnce(Vec<home::HomeShowcaseBatch>) + Clone> Completion<F> {
    fn deliver(self, index: usize, batch: home::HomeShowcaseBatch) {
        self.results.lock().unwrap().push((index, batch));
        if self.remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
            let mut batches = std::mem::take(&mut *self.results.lock().unwrap());
            batches.sort_by_key(|(index, _)| *index);
            (self.finish)(batches.into_iter().map(|(_, batch)| batch).collect());
        }
    }
}

fn request<F: FnOnce(Vec<home::HomeShowcaseBatch>) + Clone + Send + 'static>(
    mut job: Job,
    url: String,
    complete: Completion<F>,
) {
    net::fetch_bytes(url, move |response| {
        // No Shared/Slint access or enrichment on network workers. Parsing and
        // projection use only the captured source and the local evidence index.
        let page = response
            .ok()
            .and_then(|bytes| home::parse_home_showcase_catalog(&bytes, &job.source.type_));
        job.cursor.record(&job.source, page, job.mode);
        let visible = visible_count(&job.source, &job.cursor.raw, job.mode, job.revision);
        let more = job
            .cursor
            .decide(job.mode, job.supports_skip, job.desired, visible);
        let _ = profiles::invoke_from_event_loop(move || {
            if !job.current() {
                return;
            }
            if more && job.allow_more() {
                if let Some(url) = next_url(&job.source, job.cursor.next_skip) {
                    request(job, url, complete);
                    return;
                }
            } else if more {
                job.cursor.stop = Stop::Changed;
            }
            let source = job.source;
            complete.deliver(job.index, (source, job.cursor.finish()));
        });
    });
}

/// Preserve the existing first request, then optionally fill grouped Home
/// content with at most three pages and 120 inspected candidates per selection.
/// Policy/inventory changes stop further requests; valid collected raw results
/// can still be projected under the current policy. Obsolete profile/surface
/// generations never deliver a batch.
pub(super) fn fetch(
    bridge: &Bridge,
    targets: Vec<(HomeCatalogSource, String)>,
    desired: usize,
    scope: Scope,
    finish: impl FnOnce(Vec<home::HomeShowcaseBatch>) + Clone + Send + 'static,
) {
    if targets.is_empty() {
        finish(vec![]);
        return;
    }
    let (settings, installed) = {
        let state = bridge.shared.lock().unwrap();
        (state.cache_settings.clone(), state.installed.clone())
    };
    let profile = profiles::capture();
    let revision = nova_providers::metadata_revision();
    if !scope.current(bridge) || !profiles::is_current(&profile) {
        return;
    }
    let mut seen = HashSet::new();
    let targets = targets
        .into_iter()
        .filter(|(source, _)| seen.insert(source.clone()))
        .collect::<Vec<_>>();
    let complete = Completion {
        results: Arc::default(),
        remaining: Arc::new(AtomicUsize::new(targets.len())),
        finish,
    };
    for (index, (source, url)) in targets.into_iter().enumerate() {
        let owner = installed
            .iter()
            .find(|addon| addon.url == source.addon_url && addon.enabled && addon.available);
        let supports_skip = owner
            .and_then(|addon| {
                addon
                    .manifest
                    .catalog_for(&source.type_, &source.catalog_id)
            })
            .is_some_and(|catalog| catalog.supports_extra("skip"));
        let job = Job {
            bridge: bridge.clone(),
            profile: profile.clone(),
            scope,
            mode: settings.catalog_grouping_mode(&grouping_key(&source)),
            revision,
            owner_generation: owner.map(|addon| addon.generation),
            source,
            supports_skip,
            desired: desired.max(1),
            index,
            cursor: Cursor::default(),
        };
        request(job, url, complete.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_config::CatalogGroupingMode::{Original, Season, Series};

    fn source() -> HomeCatalogSource {
        HomeCatalogSource {
            addon_url: "https://owner.invalid/configuration/manifest.json".into(),
            type_: "anime".into(),
            catalog_id: "new".into(),
            genre: "Drama".into(),
        }
    }
    fn preview(id: usize, parent: usize, season: usize) -> MetaPreview {
        serde_json::from_value(serde_json::json!({"id":format!("mal:{id}"),"type":"anime","name":format!("Native {id}"),"poster":format!("https://images.invalid/{id}.jpg"),"novaCatalogGrouping":{"version":1,"kind":"series_member","parentIds":[format!("tt{parent}")],"season":season}})).unwrap()
    }
    fn page(values: Vec<MetaPreview>) -> Option<Vec<MetaPreview>> {
        Some(values)
    }

    #[test]
    fn original_keeps_one_page_while_series_backfills_by_raw_cursor_with_original_ids() {
        let source = source();
        let first = vec![preview(1, 1, 1), preview(2, 1, 2)];
        let mut original = Cursor::default();
        original.record(&source, page(first.clone()), Original);
        assert!(!original.decide(Original, true, 5, original.raw.len()));
        assert_eq!(original.stop, Stop::Original);
        assert_eq!(original.pages, 1);
        let mut grouped = Cursor::default();
        grouped.record(&source, page(first), Series);
        let count = visible_count(&source, &grouped.raw, Series, u64::MAX);
        assert_eq!(count, 1);
        assert!(grouped.decide(Series, true, 2, count));
        assert_eq!(grouped.next_skip, 2);
        let url = next_url(&source, grouped.next_skip).unwrap();
        assert!(url.contains("skip=2"));
        assert!(url.contains("genre=Drama"));
        assert!(url.starts_with("https://owner.invalid/configuration/"));
        grouped.record(
            &source,
            page(vec![preview(2, 1, 2), preview(3, 3, 1)]),
            Series,
        );
        assert_eq!(
            grouped.next_skip, 4,
            "the duplicated native source still consumes a raw offset"
        );
        assert_eq!(grouped.raw.len(), 3);
        let count = visible_count(&source, &grouped.raw, Series, u64::MAX);
        assert!(!grouped.decide(Series, true, 2, count));
        assert_eq!(grouped.stop, Stop::Enough);
        let raw = grouped.finish().unwrap();
        assert_eq!(
            raw.iter()
                .map(|preview| preview.id.as_str())
                .collect::<Vec<_>>(),
            ["mal:1", "mal:2", "mal:3"]
        );
        assert!(
            raw.iter()
                .all(|preview| preview.extra["novaSourceUrl"] == source.addon_url)
        );
    }

    #[test]
    fn collapsed_pages_have_a_three_page_and_120_raw_candidate_budget() {
        let source = source();
        let mut cursor = Cursor::default();
        for index in 0..PAGE_LIMIT {
            cursor.record(&source, page(vec![preview(index + 1, 1, 1)]), Series);
            let count = visible_count(&source, &cursor.raw, Series, u64::MAX);
            assert_eq!(
                cursor.decide(Series, true, 20, count),
                index + 1 < PAGE_LIMIT
            );
        }
        assert_eq!(cursor.stop, Stop::Budget);
        assert_eq!(cursor.pages, 3);
        let mut cursor = Cursor::default();
        cursor.record(
            &source,
            page((0..130).map(|id| preview(id + 1, 1, 1)).collect()),
            Series,
        );
        assert_eq!(cursor.next_skip, 130);
        assert_eq!(cursor.raw.len(), RAW_LIMIT);
        assert!(!cursor.decide(Series, true, 20, 1));
        assert_eq!(cursor.stop, Stop::Budget);
        let mut cursor = Cursor::default();
        cursor.record(&source, page(vec![preview(1, 1, 1)]), Season);
        assert!(!cursor.decide(Season, false, 20, 1));
        assert_eq!(cursor.stop, Stop::Unsupported);
    }

    #[test]
    fn empty_repeated_and_failed_pages_have_distinct_results_and_do_not_lose_valid_raw() {
        let source = source();
        let mut empty = Cursor::default();
        empty.record(&source, page(vec![]), Series);
        assert_eq!(empty.stop, Stop::Empty);
        assert!(empty.finish().unwrap().is_empty());
        let mut failure = Cursor::default();
        failure.record(&source, None, Series);
        assert_eq!(failure.stop, Stop::Failed);
        assert!(failure.finish().is_none());
        let mut repeated = Cursor::default();
        repeated.record(
            &source,
            page(vec![preview(1, 1, 1), preview(2, 1, 2)]),
            Series,
        );
        repeated.record(
            &source,
            page(vec![preview(2, 1, 2), preview(1, 1, 1)]),
            Series,
        );
        assert_eq!(repeated.stop, Stop::Repeated);
        assert_eq!(repeated.next_skip, 4);
        assert_eq!(repeated.raw.len(), 2);
        let mut partial = Cursor::default();
        partial.record(&source, page(vec![preview(1, 1, 1)]), Series);
        partial.record(&source, None, Series);
        assert_eq!(
            partial.next_skip, 1,
            "a failed response never consumes its cursor"
        );
        assert_eq!(partial.stop, Stop::Failed);
        assert_eq!(partial.finish().unwrap().len(), 1);
    }

    #[test]
    fn fake_transport_observes_the_supported_input_vertical_slice_without_metadata_requests() {
        let source = source();
        let responses = [
            serde_json::json!({"metas":[preview(1,1,1),preview(2,1,2)]}),
            serde_json::json!({"metas":[preview(3,3,1)]}),
        ];
        let mut requests = vec![];
        let mut cursor = Cursor::default();
        for response in responses {
            requests.push(next_url(&source, cursor.next_skip).unwrap());
            let bytes = serde_json::to_vec(&response).unwrap();
            cursor.record(
                &source,
                home::parse_home_showcase_catalog(&bytes, &source.type_),
                Series,
            );
            let count = visible_count(&source, &cursor.raw, Series, u64::MAX);
            if !cursor.decide(Series, true, 2, count) {
                break;
            }
        }
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("skip=0"));
        assert!(requests[1].contains("skip=2"));
        assert_eq!(cursor.stop, Stop::Enough);
        assert_eq!(
            cursor.raw[0].poster.as_deref(),
            Some("https://images.invalid/1.jpg")
        );
        assert_eq!(visible_count(&source, &cursor.raw, Original, u64::MAX), 3);
    }
}

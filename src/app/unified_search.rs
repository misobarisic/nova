//! Profile-local search documents and exact result actions. Only the text the
//! user types is passed to catalog search; notes and paths stay on this device.
use super::*;

const ACTION: &str = "novaLocalSearchAction";
const LABEL: &str = "novaSearchLabel";
const CONTEXT: &str = "novaSearchContext";
const LIMIT: usize = 150;
pub(super) const DEFAULT_SCOPE: i32 = 1;

pub(super) struct State {
    revision: u64,
    query: String,
    scope: i32,
    results: Vec<MetaPreview>,
    library_membership: HashMap<String, HashSet<String>>,
    library_membership_count: usize,
}

impl Default for State {
    fn default() -> Self {
        Self {
            revision: 0,
            query: String::new(),
            scope: DEFAULT_SCOPE,
            results: Vec::new(),
            library_membership: HashMap::new(),
            library_membership_count: 0,
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
            && entries
                .iter()
                .all(|entry| self.contains_saved(&entry.type_, &entry.id))
    }

    fn remember_library(&mut self, entries: &[LibraryEntry]) {
        self.library_membership.clear();
        self.library_membership_count = entries.len();
        for entry in entries {
            self.library_membership
                .entry(entry.type_.clone())
                .or_default()
                .insert(entry.id.clone());
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

fn global_id(id: &str) -> bool {
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

fn saved_documents<'a>(
    query: &'a str,
    entries: &'a [LibraryEntry],
) -> impl Iterator<Item = MetaPreview> + 'a {
    let owner = profiles::ProfileStore::capture();
    entries
        .iter()
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

fn collect_results(
    query: &str,
    entries: &[LibraryEntry],
    known: &[MetaPreview],
    jobs: Vec<DownloadJob>,
    shared: &profile_session::SessionState,
) -> Vec<MetaPreview> {
    let owner = profiles::ProfileStore::capture();
    let mut results = ScopedResults::default();
    for hit in saved_documents(query, entries) {
        // Apply movie upgrades before the bounded selection, so changing a
        // caption cannot discard a previously truncated runner-up title.
        if !jobs.iter().any(|job| {
            job.media_type == "movie"
                && job.media_type == hit.type_
                && job.media_id == hit.id
                && usable(job, &owner)
                && matches(query, &job.title, "")
        }) {
            results.insert(hit, true);
        }
    }
    let saved: HashSet<_> = entries
        .iter()
        .map(|entry| (entry.type_.as_str(), entry.id.as_str()))
        .collect();
    let mut titles: HashMap<_, _> = known
        .iter()
        .cloned()
        .map(|meta| ((meta.type_.clone(), meta.id.clone()), meta))
        .collect();
    for entry in entries {
        titles.insert(
            (entry.type_.clone(), entry.id.clone()),
            library_preview(entry),
        );
    }
    let bookmarks = bookmarks::load_bookmarks().unwrap_or_default();
    for (key, rows) in &bookmarks {
        let Ok((kind, id, episode)) = serde_json::from_str::<(String, String, String)>(key) else {
            continue;
        };
        if kind.is_empty() || id.is_empty() || episode.is_empty() {
            continue;
        }
        let metadata = rows
            .iter()
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
    for job in jobs
        .iter()
        .filter(|job| smart_offline::allows_download(&owner, job))
    {
        titles
            .entry((job.media_type.clone(), job.media_id.clone()))
            .or_insert_with(|| MetaPreview {
                id: job.media_id.clone(),
                type_: job.media_type.clone(),
                name: job.title.clone(),
                ..Default::default()
            });
    }

    let job_groups = jobs
        .iter()
        .fold(HashMap::<_, Vec<_>>::new(), |mut groups, job| {
            groups
                .entry((job.media_type.clone(), job.media_id.clone()))
                .or_default()
                .push(job);
            groups
        });
    let bookmark_groups = bookmarks
        .iter()
        .filter_map(|(key, rows)| {
            let (kind, id, episode) = serde_json::from_str::<(String, String, String)>(key).ok()?;
            (!kind.is_empty() && !id.is_empty() && !episode.is_empty())
                .then_some(((kind, id), (key, episode, rows)))
        })
        .fold(HashMap::<_, Vec<_>>::new(), |mut groups, (title, row)| {
            groups.entry(title).or_default().push(row);
            groups
        });
    // Metadata caches are device-wide, unlike notes/history. Only index a
    // parent evidenced in this profile. Load one inventory at a time rather
    // than retaining every episode in this profile's known-title corpus.
    for ((kind, id), preview) in &titles {
        if !profile_policy::allows_browse_title(&owner, kind, id) {
            continue;
        }
        let videos = read_episodes_cache_for(kind, id).unwrap_or_default();
        for video in &videos {
            if video.id.is_empty() {
                continue;
            }
            let hidden = spoiler::hidden(&shared.lock().unwrap(), kind, id, &video.id);
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
            .filter(|job| usable(job, &owner))
        {
            let video = videos.iter().find(|video| video.id == job.request_id);
            let hidden = spoiler::hidden(
                &shared.lock().unwrap(),
                &job.media_type,
                &job.media_id,
                &job.request_id,
            );
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
            let hidden = spoiler::hidden(&shared.lock().unwrap(), kind, id, episode_id);
            let episode = videos
                .iter()
                .find(|video| video.id == *episode_id)
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
            for bookmark in rows
                .iter()
                .filter(|bookmark| bookmark.position.is_finite() && bookmark.position >= 0.0)
            {
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
    results.finish()
}

pub(super) fn merge(state: &Shared, remote: Vec<MetaPreview>) -> Vec<MetaPreview> {
    let local = &state.unified_search;
    let mut results: Vec<_> = local
        .results
        .iter()
        .filter(|meta| match (local.scope, action(meta)) {
            (0, _) => true,
            (2, Some(Action::Title)) => true,
            (2, Some(Action::Download { .. })) => state
                .entries
                .iter()
                .any(|entry| entry.type_ == meta.type_ && entry.id == title_id(meta)),
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
        let saved_titles: HashSet<_> = state
            .entries
            .iter()
            .map(|entry| (entry.type_.as_str(), entry.id.as_str()))
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
                    if global_id(&meta.id)
                        && saved_titles.contains(&(meta.type_.as_str(), meta.id.as_str()))
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
    /// Library is published for progress updates too. Borrowed membership checks
    /// keep those unchanged publications free of corpus rebuilds and workers.
    pub(super) fn refresh_unified_search_library(&self) {
        let Some(app) = self.app() else { return };
        if !app.get_discover_search_open() {
            return;
        }
        let query = {
            let state = self.shared.lock().unwrap();
            if state.unified_search.library_matches(&state.entries) {
                return;
            }
            state.unified_search.query.clone()
        };
        self.refresh_local_search_inner(&query, true);
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

    fn refresh_local_search_inner(&self, query: &str, retain_documents: bool) {
        let query = query.trim().to_owned();
        let (revision, entries, known) = {
            let mut state = self.shared.lock().unwrap();
            let entries = state.entries.clone();
            state.unified_search.remember_library(&entries);
            state.unified_search.revision = state.unified_search.revision.wrapping_add(1);
            state.unified_search.query = query.clone();
            state.unified_search.results = if query.chars().count() < 2 {
                Vec::new()
            } else if retain_documents {
                // Only saved-title membership changes synchronously. Retain
                // episode/download/note rows until the profile-owned worker
                // refreshes them, so existing targets do not blink away.
                let mut retained = ScopedResults::default();
                for meta in saved_documents(&query, &entries) {
                    retained.insert(meta, true);
                }
                for meta in std::mem::take(&mut state.unified_search.results) {
                    if !matches!(action(&meta), Some(Action::Title)) {
                        let saved = state
                            .unified_search
                            .contains_saved(&meta.type_, title_id(&meta));
                        retained.insert(meta, saved);
                    }
                }
                retained.finish()
            } else {
                saved_results(&query, &entries)
            };
            let mut known = state.previews.clone();
            known.extend(state.progress.values().filter_map(|progress| {
                let history = progress.history.as_ref()?;
                (!history.type_.is_empty() && !history.title.is_empty()).then(|| MetaPreview {
                    id: progress.series_id.clone(),
                    type_: history.type_.clone(),
                    name: history.title.clone(),
                    poster: (!history.poster_url.is_empty()).then(|| history.poster_url.clone()),
                    ..Default::default()
                })
            }));
            (state.unified_search.revision, entries, known)
        };
        self.repaint_search();
        if query.chars().count() < 2 {
            return;
        }
        let bridge = self.clone();
        thread::spawn(move || {
            let results = collect_results(
                &query,
                &entries,
                &known,
                bridge.downloads.jobs(),
                &bridge.shared,
            );
            let _ = profiles::invoke_from_event_loop(move || {
                {
                    let mut state = bridge.shared.lock().unwrap();
                    if state.unified_search.revision != revision
                        || state.unified_search.query != query
                    {
                        return;
                    }
                    state.unified_search.results = results;
                }
                bridge.repaint_search();
            });
        });
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
                    let query = self.shared.lock().unwrap().unified_search.query.clone();
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
mod tests {
    use super::*;

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
        state.unified_search.results = vec![saved.clone(), episode.clone()];
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
        state.unified_search.results = published;
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
        let (hi, _) = std::sync::mpsc::channel();
        let (lo, _) = std::sync::mpsc::channel();
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
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
                results: vec![first.clone(), second.clone()],
            }];
        }
        bridge.refresh_local_search("Signal");
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
            state.unified_search.results = saved_results("Office", &long_entries);
            assert_eq!(merge(&state, vec![]).len(), 1, "immediate Library result");
            let published = collect_results("Office", &long_entries, &[], vec![long_job.clone()], &shared);
            assert!(published.len() <= LIMIT * 5);
            state.unified_search.results = published.clone();
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

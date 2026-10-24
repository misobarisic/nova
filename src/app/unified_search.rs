//! Profile-local search documents and exact result actions. Only the text the
//! user types is passed to catalog search; notes and paths stay on this device.
use super::*;

const ACTION: &str = "novaLocalSearchAction";
const LABEL: &str = "novaSearchLabel";
const CONTEXT: &str = "novaSearchContext";
const LIMIT: usize = 150;

#[derive(Default)]
pub(super) struct State {
    revision: u64,
    query: String,
    scope: i32,
    results: Vec<MetaPreview>,
}

impl State {
    pub(super) fn clear_scope(&mut self) {
        self.scope = 0;
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

fn saved_results(query: &str, entries: &[LibraryEntry]) -> Vec<MetaPreview> {
    let owner = profiles::ProfileStore::capture();
    entries
        .iter()
        .filter(|entry| {
            matches(query, &entry.name, &entry.year)
                && profile_policy::allows_browse_title(&owner, &entry.type_, &entry.id)
        })
        .map(|entry| {
            document(
                library_preview(entry),
                entry.id.clone(),
                entry.name.clone(),
                "Saved · Open title",
                entry.year.clone(),
                Action::Title,
            )
        })
        .collect()
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
    let mut results = saved_results(query, entries);
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

    // Metadata caches are device-wide, unlike notes/history. Only index a
    // parent evidenced in this profile; scanning every cache would surface
    // another profile's otherwise private browsing activity.
    let inventories: Vec<_> = titles
        .keys()
        .filter_map(|(kind, id)| {
            if !profile_policy::allows_browse_title(&owner, kind, id) {
                return None;
            }
            let videos = read_episodes_cache_for(kind, id)?;
            Some((kind.clone(), id.clone(), videos))
        })
        .collect();
    let mut episode_cache = HashMap::new();
    for (kind, id, videos) in inventories {
        let preview = titles
            .get(&(kind.clone(), id.clone()))
            .cloned()
            .unwrap_or_else(|| MetaPreview {
                id: id.clone(),
                type_: kind.clone(),
                name: id.clone(),
                ..Default::default()
            });
        for video in &videos {
            if video.id.is_empty() {
                continue;
            }
            let hidden = spoiler::hidden(&shared.lock().unwrap(), &kind, &id, &video.id);
            let label = if hidden {
                episode_se_label(video)
            } else {
                stream_episode_context_label(video)
            };
            if matches(query, &label, &preview.title()) {
                results.push(document(
                    preview.clone(),
                    episode_result_id(&kind, &id, &video.id),
                    label,
                    "Episode · Find sources",
                    preview.title(),
                    Action::Episode {
                        title: id.clone(),
                        video: video.clone(),
                    },
                ));
            }
        }
        episode_cache.insert((kind, id), videos);
    }
    for job in jobs.iter().filter(|job| usable(job, &owner)) {
        let preview = titles
            .get(&(job.media_type.clone(), job.media_id.clone()))
            .cloned()
            .unwrap_or_else(|| MetaPreview {
                id: job.media_id.clone(),
                type_: job.media_type.clone(),
                name: job.title.clone(),
                ..Default::default()
            });
        let video = episode_cache
            .get(&(job.media_type.clone(), job.media_id.clone()))
            .and_then(|videos| videos.iter().find(|video| video.id == job.request_id));
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
            preview,
            id,
            name,
            "Available offline · Play",
            episode,
            Action::Download {
                job: job.id.clone(),
                title: job.media_id.clone(),
            },
        );
        if job.media_type == "movie"
            && let Some(existing) = results
                .iter_mut()
                .find(|meta| meta.type_ == hit.type_ && meta.id == hit.id)
        {
            *existing = hit;
            continue;
        }
        results.push(hit);
    }
    for (key, rows) in bookmarks {
        let Ok((kind, id, episode_id)) = serde_json::from_str::<(String, String, String)>(&key)
        else {
            continue;
        };
        if !profile_policy::allows_browse_title(&owner, &kind, &id) {
            continue;
        }
        let Some(preview) = titles.get(&(kind.clone(), id.clone())) else {
            continue;
        };
        let hidden = spoiler::hidden(&shared.lock().unwrap(), &kind, &id, &episode_id);
        let episode = episode_cache
            .get(&(kind.clone(), id.clone()))
            .and_then(|videos| videos.iter().find(|video| video.id == episode_id))
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
            let scene = bookmarks::scene_id(&key, bookmark);
            results.push(document(
                preview.clone(),
                format!("local:bookmark:{scene}"),
                if bookmark.note.is_empty() {
                    preview.title()
                } else {
                    bookmark.note.clone()
                },
                "Bookmark · Resume here",
                context,
                Action::Bookmark {
                    scene,
                    title: id.clone(),
                },
            ));
        }
    }
    // Deterministic output keeps focus and artwork attached during incremental
    // catalog delivery. Never collapse unproven equal-looking display names.
    results.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    results.truncate(LIMIT);
    results
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
        .cloned()
        .collect();
    if local.scope <= 1 {
        let local_titles: HashSet<_> = results
            .iter()
            .filter(|meta| matches!(action(meta), Some(Action::Title | Action::Download { .. })))
            .map(|meta| (meta.type_.clone(), title_id(meta).to_owned()))
            .collect();
        results.extend(remote.into_iter().filter(|meta| {
            !global_id(&meta.id) || !local_titles.contains(&(meta.type_.clone(), meta.id.clone()))
        }));
    }
    results
}

pub(super) fn card_text(meta: &MetaPreview) -> (String, String) {
    let label = meta
        .extra
        .get(LABEL)
        .and_then(|value| value.as_str())
        .unwrap_or("Catalog · Find sources");
    let context = meta
        .extra
        .get(CONTEXT)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| meta.year_str().unwrap_or_default());
    (text::tr(label).into(), context)
}

impl Bridge {
    pub(super) fn refresh_unified_search_presentation(&self) {
        if let Some(app) = self.app()
            && app.get_discover_search_open()
        {
            self.refresh_local_search(&app.get_search_text());
        }
    }

    pub(super) fn refresh_local_search(&self, query: &str) {
        let query = query.trim().to_owned();
        let (revision, entries, known) = {
            let mut state = self.shared.lock().unwrap();
            state.unified_search.revision = state.unified_search.revision.wrapping_add(1);
            state.unified_search.query = query.clone();
            state.unified_search.results = if query.chars().count() >= 2 {
                saved_results(&query, &state.entries)
            } else {
                Vec::new()
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
            (state.unified_search.revision, state.entries.clone(), known)
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
            "Saved · Open title",
            String::new(),
            Action::Title,
        );
        let episode = document(
            a.clone(),
            "local:episode:one".into(),
            "S1 E2".into(),
            "Episode · Find sources",
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
            merge(&state, vec![a.clone()]).len(),
            2,
            "title dedups while episode stays separate"
        );
        state.unified_search.scope = 1;
        assert_eq!(merge(&state, vec![a.clone()])[0].id, a.id);
        state.unified_search.scope = 3;
        assert_eq!(merge(&state, vec![a])[0].id, episode.id);
        state.unified_search.scope = 5;
        assert!(merge(&state, vec![]).is_empty());
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
            assert!(matches!(action(&notes[0]), Some(Action::Bookmark { scene: found, .. }) if found == scene));
            let numbered = collect("S1 E2");
            assert_eq!(numbered.iter().filter(|meta| matches!(action(meta), Some(Action::Episode { .. }))).count(), 1);
            assert_eq!(numbered.iter().filter(|meta| matches!(action(meta), Some(Action::Download { .. }))).count(), 1);
            assert!(numbered.iter().all(|meta| !meta.name.contains("Hidden finale")));
            assert!(!usable(&missing, &alice));
            assert!(!usable(&paused, &alice));
            assert!(usable(&good, &alice));
            let policy = profile_policy::ProfilePolicy { restricted: true, ..Default::default() };
            alice.write(profile_policy::KEY, &serde_json::to_string(&policy).unwrap()).unwrap();
            assert!(collect("Local").is_empty(), "restricted profiles cannot search disallowed titles");
            assert!(!usable(&good, &alice));
        });
    }
}

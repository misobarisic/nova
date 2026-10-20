//! Profile-owned scene archive. Old bookmarks retain an unknown added date;
//! editing a note never changes the original date or the playback position.
use super::*;
use crate::{BookmarkSceneRow, BookmarkTitleRow, BookmarksBridge};

pub(super) const BOOKMARKS_KEY: &str = "scene_bookmarks:v1";
pub(super) type Bookmarks = HashMap<String, Vec<Bookmark>>;
static NEXT_BOOKMARK: AtomicU64 = AtomicU64::new(1);
static ARTWORK_RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub(super) struct BookmarkMetadata {
    pub media_type: String,
    pub title_id: String,
    pub episode_id: String,
    pub title: String,
    pub episode: String,
    pub poster_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(super) struct Bookmark {
    #[serde(default)]
    pub id: String,
    pub position: f64,
    pub note: String,
    #[serde(default)]
    pub added_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BookmarkMetadata>,
}

#[derive(Default)]
pub(super) struct State {
    artwork_requested: HashSet<String>,
}

pub(super) fn identity_key(identity: &nova_playback_api::MediaIdentity) -> String {
    serde_json::to_string(&(
        &identity.media_type,
        &identity.title_id,
        &identity.episode_id,
    ))
    .expect("bookmark identity")
}

pub(super) fn clean_note(note: &str) -> String {
    note.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(280)
        .collect()
}

pub(super) fn new_bookmark(
    position: f64,
    note: &str,
    identity: &nova_playback_api::MediaIdentity,
    poster_url: &str,
) -> Bookmark {
    let added_at = nova_config::now_ms();
    Bookmark {
        id: format!(
            "{:x}-{:x}-{:x}-{:x}",
            nova_sync::local_device().unwrap_or(u64::from(std::process::id())),
            added_at,
            position.to_bits(),
            NEXT_BOOKMARK.fetch_add(1, Ordering::Relaxed)
        ),
        position,
        note: clean_note(note),
        added_at,
        metadata: Some(BookmarkMetadata {
            media_type: identity.media_type.clone(),
            title_id: identity.title_id.clone(),
            episode_id: identity.episode_id.clone(),
            title: identity.title.clone(),
            episode: identity.episode.clone(),
            poster_url: poster_url.to_owned(),
        }),
    }
}

pub(super) fn bookmark_id(bookmark: &Bookmark) -> String {
    if bookmark.id.is_empty() {
        format!("legacy-{:016x}", bookmark.position.to_bits())
    } else {
        bookmark.id.clone()
    }
}

pub(super) fn scene_id(key: &str, bookmark: &Bookmark) -> String {
    serde_json::to_string(&(key, bookmark_id(bookmark))).expect("scene identity")
}

pub(super) fn load_bookmarks() -> Result<Bookmarks, String> {
    storage::try_get_str(BOOKMARKS_KEY)
        .map_err(|e| e.to_string())?
        .map_or_else(
            || Ok(HashMap::new()),
            |raw| serde_json::from_str(&raw).map_err(|e| e.to_string()),
        )
}

pub(super) fn save_bookmarks(values: &Bookmarks) -> Result<(), String> {
    if !writable_key(BOOKMARKS_KEY) {
        return Err("bookmark storage is not writable".into());
    }
    let raw = serde_json::to_string(values).map_err(|e| e.to_string())?;
    sync::persist_sync_snapshot(BOOKMARKS_KEY, &raw, false).map_err(|e| e.to_string())
}

fn valid(bookmark: &Bookmark) -> bool {
    bookmark.position.is_finite() && bookmark.position >= 0.0
}
fn title_key(kind: &str, title: &str) -> String {
    serde_json::to_string(&(kind, title)).expect("title identity")
}
fn added(at: u64) -> String {
    if at == 0 {
        text::tr("Added date unknown").into()
    } else {
        text::journal_date(at / 1000, true)
    }
}

#[derive(Clone)]
struct Group {
    id: String,
    kind: String,
    title: String,
    poster: String,
    latest: u64,
    scenes: Vec<(String, nova_playback_api::MediaIdentity, Bookmark)>,
}

fn groups(
    values: &Bookmarks,
    entries: &[LibraryEntry],
    progress: &HashMap<String, EpisodeProgress>,
) -> Vec<Group> {
    let mut groups = std::collections::BTreeMap::<String, Group>::new();
    for (key, bookmarks) in values {
        let Ok((kind, title_id, episode_id)) =
            serde_json::from_str::<(String, String, String)>(key)
        else {
            continue;
        };
        if kind.is_empty() || title_id.is_empty() || episode_id.is_empty() {
            continue;
        }
        let library = entries
            .iter()
            .find(|entry| entry.type_ == kind && entry.id == title_id);
        let history = progress
            .get(&progress_map_key(&title_id, &episode_id))
            .and_then(|progress| progress.history.as_ref());
        for bookmark in bookmarks.iter().filter(|bookmark| valid(bookmark)) {
            let metadata = bookmark.metadata.as_ref().filter(|meta| {
                meta.media_type == kind
                    && meta.title_id == title_id
                    && meta.episode_id == episode_id
            });
            let title = metadata
                .map(|meta| meta.title.as_str())
                .filter(|title| !title.is_empty())
                .or_else(|| library.map(|entry| entry.name.as_str()))
                .or_else(|| history.map(|history| history.title.as_str()))
                .unwrap_or(text::tr("Untitled saved title"))
                .to_owned();
            let poster = metadata
                .map(|meta| meta.poster_url.as_str())
                .filter(|url| !url.is_empty())
                .or_else(|| library.map(|entry| entry.poster_url.as_str()))
                .or_else(|| history.map(|history| history.poster_url.as_str()))
                .unwrap_or_default()
                .to_owned();
            let episode = metadata
                .map(|meta| meta.episode.as_str())
                .filter(|value| !value.is_empty())
                .or_else(|| history.map(|history| history.episode_label.as_str()))
                .unwrap_or_default()
                .to_owned();
            let id = title_key(&kind, &title_id);
            let group = groups.entry(id.clone()).or_insert_with(|| Group {
                id,
                kind: kind.clone(),
                title: title.clone(),
                poster: poster.clone(),
                latest: 0,
                scenes: Vec::new(),
            });
            if bookmark.added_at >= group.latest {
                group.latest = bookmark.added_at;
                group.title.clone_from(&title);
                if !poster.is_empty() {
                    group.poster.clone_from(&poster);
                }
            }
            group.scenes.push((
                scene_id(key, bookmark),
                nova_playback_api::MediaIdentity {
                    media_type: kind.clone(),
                    title_id: title_id.clone(),
                    episode_id: episode_id.clone(),
                    title,
                    episode,
                },
                bookmark.clone(),
            ));
        }
    }
    groups.into_values().collect()
}

fn sort_groups(groups: &mut [Group], sort: i32) {
    groups.sort_by(|a, b| {
        let title = || {
            a.title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        };
        if sort == 1 {
            title()
        } else {
            b.latest.cmp(&a.latest).then_with(title)
        }
    });
}

impl Bridge {
    pub(super) fn setup_bookmarks(&self) {
        let Some(app) = self.app() else { return };
        let global = app.global::<BookmarksBridge>();
        let b = self.clone();
        global.on_opened(move || {
            b.shared.lock().unwrap().bookmarks.artwork_requested.clear();
            if let Some(app) = b.app() {
                app.global::<BookmarksBridge>().set_notice("".into());
            }
            b.refresh_bookmarks();
        });
        let b = self.clone();
        global.on_query_changed(move |query| {
            if let Some(app) = b.app() {
                app.global::<BookmarksBridge>().set_query(query);
            }
            b.refresh_bookmarks();
        });
        let b = self.clone();
        global.on_sort_changed(move |sort| {
            if let Some(app) = b.app() {
                app.global::<BookmarksBridge>()
                    .set_sort(if sort == 1 { 1 } else { 0 });
            }
            b.refresh_bookmarks();
        });
        let b = self.clone();
        global.on_select_title(move |id| {
            if let Some(app) = b.app() {
                app.global::<BookmarksBridge>().set_title_id(id);
            }
            b.refresh_bookmarks();
        });
        let b = self.clone();
        global.on_edit(move |id, note| b.edit_saved_bookmark(&id, Some(&note)));
        let b = self.clone();
        global.on_remove(move |id| {
            b.edit_saved_bookmark(&id, None);
        });
        let b = self.clone();
        global.on_play(move |id| b.play_saved_scene(&id));
    }

    fn bookmark_groups(&self) -> Result<Vec<Group>, String> {
        let values = load_bookmarks()?;
        let state = self.shared.lock().unwrap();
        Ok(groups(&values, &state.entries, &state.progress))
    }

    fn bookmark_notice(&self, notice: &str) {
        if let Some(app) = self.app() {
            app.global::<BookmarksBridge>()
                .set_notice(text::tr(notice).into());
        }
    }

    pub(super) fn refresh_bookmarks(&self) {
        let Some(app) = self.app() else { return };
        let global = app.global::<BookmarksBridge>();
        let mut groups = match self.bookmark_groups() {
            Ok(groups) => groups,
            Err(_) => {
                global.set_titles(Rc::new(VecModel::<BookmarkTitleRow>::default()).into());
                global.set_scenes(Rc::new(VecModel::<BookmarkSceneRow>::default()).into());
                self.bookmark_notice(
                    "Bookmarks could not be loaded. Saved data has been preserved.",
                );
                return;
            }
        };
        sort_groups(&mut groups, global.get_sort());
        let selected = global.get_title_id();
        let selected_group = groups
            .iter_mut()
            .find(|group| group.id == selected.as_str());
        let mut scenes = Vec::new();
        if let Some(group) = selected_group {
            global.set_title_caption(group.title.clone().into());
            group.scenes.sort_by(|a, b| {
                a.1.episode
                    .cmp(&b.1.episode)
                    .then_with(|| a.2.position.total_cmp(&b.2.position))
                    .then_with(|| a.0.cmp(&b.0))
            });
            scenes = group
                .scenes
                .iter()
                .map(|(id, identity, bookmark)| BookmarkSceneRow {
                    id: id.clone().into(),
                    title: if identity.media_type == "movie" {
                        text::tr("Movie").into()
                    } else if identity.episode.is_empty() {
                        text::tr("Episode").into()
                    } else {
                        identity.episode.clone().into()
                    },
                    position: text::journal_duration(bookmark.position).into(),
                    note: bookmark.note.clone().into(),
                    added: added(bookmark.added_at).into(),
                })
                .collect();
        } else {
            global.set_title_id("".into());
            global.set_title_caption("".into());
        }
        global.set_scenes(Rc::new(VecModel::from(scenes)).into());
        let query = global.get_query().to_lowercase();
        let visible: Vec<_> = groups
            .into_iter()
            .filter(|group| {
                query.is_empty()
                    || group.title.to_lowercase().contains(&query)
                    || group.scenes.iter().any(|(_, identity, bookmark)| {
                        identity.episode.to_lowercase().contains(&query)
                            || bookmark.note.to_lowercase().contains(&query)
                    })
            })
            .collect();
        let rows = visible
            .iter()
            .map(|group| {
                let pixels =
                    decoded_cache_get(&image_cache_key(&group.poster, ImageUse::DetailPoster));
                BookmarkTitleRow {
                    id: group.id.clone().into(),
                    title: group.title.clone().into(),
                    context: text::tr(if group.kind == "movie" {
                        "Movie"
                    } else {
                        "Series"
                    })
                    .into(),
                    added: added(group.latest).into(),
                    count: text::bookmarks_count(group.scenes.len()).into(),
                    has_poster: pixels.is_some(),
                    poster: pixels.map(Image::from_rgba8).unwrap_or_default(),
                }
            })
            .collect::<Vec<_>>();
        global.set_titles(Rc::new(VecModel::from(rows)).into());
        if global.get_open() {
            self.load_bookmark_artwork(visible);
        }
    }

    fn edit_saved_bookmark(&self, scene: &str, note: Option<&str>) -> bool {
        let Ok((key, id)) = serde_json::from_str::<(String, String)>(scene) else {
            return false;
        };
        let Ok(mut values) = load_bookmarks() else {
            self.bookmark_notice("Bookmarks could not be loaded. Saved data has been preserved.");
            return false;
        };
        let Some(rows) = values.get_mut(&key) else {
            return false;
        };
        let Some(index) = rows.iter().position(|bookmark| bookmark_id(bookmark) == id) else {
            return false;
        };
        if let Some(note) = note {
            rows[index].note = clean_note(note);
        } else {
            rows.remove(index);
        }
        if rows.is_empty() {
            values.remove(&key);
        }
        if save_bookmarks(&values).is_err() {
            self.bookmark_notice("Bookmarks could not be saved. Please try again.");
            return false;
        }
        self.bookmark_notice("");
        self.reload_scene_bookmarks();
        self.refresh_bookmarks();
        true
    }

    fn play_saved_scene(&self, id: &str) {
        let Some((_, identity, bookmark)) = self
            .bookmark_groups()
            .ok()
            .into_iter()
            .flatten()
            .flat_map(|group| group.scenes)
            .find(|(scene, _, _)| scene == id)
        else {
            return;
        };
        let poster = bookmark
            .metadata
            .as_ref()
            .map(|meta| meta.poster_url.clone())
            .filter(|url| !url.is_empty());
        if let Some(app) = self.app() {
            app.global::<BookmarksBridge>().set_open(false);
        }
        self.open_preview(
            MetaPreview {
                id: identity.title_id.clone(),
                type_: identity.media_type.clone(),
                name: identity.title.clone(),
                poster,
                ..Default::default()
            },
            0,
            false,
            None,
            false,
        );
        self.arm_bookmark_resume(identity.clone(), bookmark.position);
        if identity.media_type == "movie" {
            self.watch_now();
        } else {
            let video = {
                let mut state = self.shared.lock().unwrap();
                let Some(modal) = state.modal_item.as_mut().filter(|modal| {
                    modal.id == identity.title_id && modal.type_ == identity.media_type
                }) else {
                    return;
                };
                if let Some(video) = modal
                    .videos
                    .iter()
                    .find(|video| video.id == identity.episode_id)
                {
                    video.clone()
                } else {
                    let video = Video {
                        id: identity.episode_id.clone(),
                        name: identity.episode.clone(),
                        ..Default::default()
                    };
                    modal.videos.push(video.clone());
                    video
                }
            };
            self.select_episode_streams(identity.title_id, identity.episode_id, video, 0);
        }
        if let Some(app) = self.app() {
            app.set_detail_deep_stream(true);
        }
    }

    fn load_bookmark_artwork(&self, groups: Vec<Group>) {
        let revision = image_settings_revision();
        let pending = {
            let mut state = self.shared.lock().unwrap();
            let keys: HashSet<_> = groups
                .iter()
                .map(|group| format!("{}:{}:{revision}", group.id, group.poster))
                .collect();
            state
                .bookmarks
                .artwork_requested
                .retain(|key| keys.contains(key));
            groups
                .into_iter()
                .filter(|group| {
                    !group.poster.is_empty()
                        && decoded_cache_get(&image_cache_key(
                            &group.poster,
                            ImageUse::DetailPoster,
                        ))
                        .is_none()
                        && !state
                            .bookmarks
                            .artwork_requested
                            .contains(&format!("{}:{}:{revision}", group.id, group.poster))
                })
                .collect::<Vec<_>>()
        };
        if pending.is_empty()
            || ARTWORK_RUNNING
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        let b = self.clone();
        thread::spawn(move || {
            for group in pending {
                let (send, receive) = std::sync::mpsc::sync_channel(1);
                let check = b.clone();
                let identity = group.id.clone();
                let poster = group.poster.clone();
                if profiles::invoke_from_event_loop(move || {
                    let visible = check.app().is_some_and(|app| {
                        let global = app.global::<BookmarksBridge>();
                        global.get_open()
                            && global.get_titles().iter().any(|row| row.id == identity)
                    });
                    let allowed = visible && image_settings_revision() == revision;
                    if allowed {
                        check
                            .shared
                            .lock()
                            .unwrap()
                            .bookmarks
                            .artwork_requested
                            .insert(format!("{identity}:{poster}:{revision}"));
                    }
                    let _ = send.send(allowed);
                })
                .is_err()
                    || receive.recv_timeout(Duration::from_secs(2)) != Ok(true)
                {
                    break;
                }
                let pixels = display_pixels(&group.poster, ImageUse::DetailPoster);
                let publish = b.clone();
                let _ = profiles::invoke_from_event_loop(move || {
                    let Some(app) = publish.app() else { return };
                    let global = app.global::<BookmarksBridge>();
                    if !global.get_open() || image_settings_revision() != revision {
                        return;
                    }
                    let model = global.get_titles();
                    let Some(rows) = model.as_any().downcast_ref::<VecModel<BookmarkTitleRow>>()
                    else {
                        return;
                    };
                    if let Some((index, mut row)) =
                        rows.iter().enumerate().find(|(_, row)| row.id == group.id)
                        && let Some(pixels) = pixels
                    {
                        row.poster = Image::from_rgba8(pixels);
                        row.has_poster = true;
                        rows.set_row_data(index, row);
                    }
                });
            }
            ARTWORK_RUNNING.store(false, Ordering::Release);
            let _ = profiles::invoke_account(move || {
                profiles::with_context(profiles::active_context(), || b.refresh_bookmarks())
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity(kind: &str, title: &str, episode: &str) -> nova_playback_api::MediaIdentity {
        nova_playback_api::MediaIdentity {
            media_type: kind.into(),
            title_id: title.into(),
            episode_id: episode.into(),
            title: "Saved title".into(),
            episode: "Episode 1".into(),
        }
    }
    #[test]
    fn legacy_bookmarks_keep_unknown_dates_and_stable_scene_identity() {
        let key = identity_key(&identity("series", "show", "episode"));
        let values: Bookmarks = serde_json::from_value(
            serde_json::json!({ (key.clone()): [{"position":42.0,"note":"old note"}] }),
        )
        .unwrap();
        let bookmark = &values[&key][0];
        assert_eq!(bookmark.added_at, 0);
        assert_eq!(bookmark.metadata, None);
        assert_eq!(scene_id(&key, bookmark), scene_id(&key, &bookmark.clone()));
        assert_eq!(groups(&values, &[], &HashMap::new()).len(), 1);
    }
    #[test]
    fn editing_can_clear_notes_without_rewriting_added_date_or_identity() {
        let mut bookmark = new_bookmark(42.0, "note", &identity("movie", "title", "title"), "");
        let before = (bookmark.id.clone(), bookmark.added_at, bookmark.position);
        bookmark.note = clean_note("   ");
        assert!(bookmark.note.is_empty());
        assert_eq!(before, (bookmark.id, bookmark.added_at, bookmark.position));
    }
    #[test]
    fn groups_use_typed_title_identity_and_sort_by_latest_addition() {
        let mut values = Bookmarks::new();
        for (kind, title, episode, name, date) in [
            ("series", "a", "1", "Zulu", 10),
            ("series", "a", "2", "Zulu", 30),
            ("movie", "a", "a", "Alpha", 20),
            ("series", "b", "1", "Beta", 0),
        ] {
            let identity = identity(kind, title, episode);
            let mut bookmark = new_bookmark(4.0, "", &identity, "");
            bookmark.added_at = date;
            bookmark.metadata.as_mut().unwrap().title = name.into();
            values.insert(identity_key(&identity), vec![bookmark]);
        }
        let mut grouped = groups(&values, &[], &HashMap::new());
        assert_eq!(grouped.len(), 3);
        sort_groups(&mut grouped, 0);
        assert_eq!(
            grouped
                .iter()
                .map(|group| group.title.as_str())
                .collect::<Vec<_>>(),
            ["Zulu", "Alpha", "Beta"]
        );
        assert_eq!(grouped[0].scenes.len(), 2);
        sort_groups(&mut grouped, 1);
        assert_eq!(
            grouped
                .iter()
                .map(|group| group.title.as_str())
                .collect::<Vec<_>>(),
            ["Alpha", "Beta", "Zulu"]
        );
    }
}

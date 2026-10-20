//! A deliberate evening plan, independent of Library membership and playback
//! history. Choosing a source never silently consumes the planned item.
use super::*;
use crate::{QueueBridge, WatchQueueRow};

const KEY: &str = "watch_queue:v1";
const MAX_ITEMS: usize = 200;
// A profile switch must not start another set of image requests while the
// outgoing profile still owns an in-flight request.
static ARTWORK_RUNNING: AtomicBool = AtomicBool::new(false);

struct ArtworkWorker;
impl Drop for ArtworkWorker {
    fn drop(&mut self) {
        ARTWORK_RUNNING.store(false, Ordering::Release);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Entry {
    type_: String,
    series_id: String,
    episode_id: String,
    title: String,
    poster_url: String,
    episode_title: String,
    season: Option<u32>,
    episode: Option<u32>,
}

impl Entry {
    fn key(&self) -> String {
        serde_json::to_string(&(&self.type_, &self.series_id, &self.episode_id))
            .expect("queue identity")
    }

    fn artwork_key(&self, revision: u64) -> String {
        serde_json::to_string(&(self.key(), &self.poster_url, revision))
            .expect("queue artwork identity")
    }

    fn context(&self) -> String {
        if self.type_ == "movie" {
            return text::tr("Movie").to_owned();
        }
        let number = match (self.season, self.episode) {
            (Some(s), Some(e)) => format!("S{s} E{e}"),
            (_, Some(e)) => format!("E{e}"),
            _ => text::tr("Episode").to_owned(),
        };
        text::stream_episode_context(&number, &self.episode_title)
    }

    fn video(&self) -> Video {
        Video {
            id: self.episode_id.clone(),
            name: self.episode_title.clone(),
            season: self.season,
            episode: self.episode,
            ..Default::default()
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SavedQueue {
    version: u32,
    entries: Vec<Entry>,
}

#[derive(Default)]
pub(super) struct State {
    entries: Option<Vec<Entry>>,
    artwork_requested: HashSet<String>,
}

fn decode(raw: &str) -> Result<Vec<Entry>, String> {
    let saved: SavedQueue = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    if saved.version != 1 || saved.entries.len() > MAX_ITEMS {
        return Err("unsupported queue data".into());
    }
    if saved.entries.iter().any(|entry| {
        entry.type_.trim().is_empty()
            || entry.series_id.trim().is_empty()
            || entry.episode_id.trim().is_empty()
    }) {
        return Err("invalid queue identity".into());
    }
    let mut seen = HashSet::new();
    Ok(saved
        .entries
        .into_iter()
        .filter(|e| seen.insert(e.key()))
        .collect())
}

fn move_entry(entries: &mut [Entry], key: &str, direction: i32) {
    if direction != -1 && direction != 1 {
        return;
    }
    let Some(index) = entries.iter().position(|entry| entry.key() == key) else {
        return;
    };
    let target = if direction < 0 {
        index.checked_sub(1)
    } else {
        index.checked_add(1).filter(|&index| index < entries.len())
    };
    if let Some(target) = target {
        entries.swap(index, target);
    }
}

impl Bridge {
    pub(super) fn setup_watch_queue(&self) {
        let Some(app) = self.app() else { return };
        let queue = app.global::<QueueBridge>();
        let b = self.clone();
        queue.on_opened(move || {
            // Failed requests can be retried when the user returns. Successful
            // requests are served from the decoded image cache.
            b.shared
                .lock()
                .unwrap()
                .watch_queue
                .artwork_requested
                .clear();
            b.refresh_watch_queue();
        });
        let b = self.clone();
        queue.on_add_detail(move || b.queue_detail());
        let b = self.clone();
        queue.on_add_episode(move |index| b.queue_episode(index));
        let b = self.clone();
        queue.on_play(move |key| b.play_queued(&key));
        let b = self.clone();
        queue.on_move(move |key, direction| {
            b.edit_watch_queue(|entries| move_entry(entries, &key, direction));
        });
        let b = self.clone();
        queue.on_remove(move |key| {
            b.edit_watch_queue(|entries| entries.retain(|entry| entry.key() != key.as_str()));
        });
        let b = self.clone();
        queue.on_clear_watched(move || {
            let watched = {
                let state = b.shared.lock().unwrap();
                state.progress.clone()
            };
            b.edit_watch_queue(|entries| {
                entries.retain(|entry| {
                    !watched
                        .get(&progress_map_key(&entry.series_id, &entry.episode_id))
                        .is_some_and(|progress| progress.watched)
                });
            });
        });
    }

    fn load_watch_queue(&self) -> bool {
        if self.shared.lock().unwrap().watch_queue.entries.is_some() {
            return true;
        }
        let result = storage::try_get_str(KEY)
            .map_err(|e| e.to_string())
            .and_then(|raw| raw.map_or_else(|| Ok(Vec::new()), |raw| decode(&raw)));
        match result {
            Ok(entries) => {
                self.shared.lock().unwrap().watch_queue.entries = Some(entries);
                if let Some(app) = self.app() {
                    app.global::<QueueBridge>().set_notice("".into());
                }
                true
            }
            Err(error) => {
                storage::report(storage::Error::new(storage::ErrorKind::Schema, error));
                if let Some(app) = self.app() {
                    app.global::<QueueBridge>().set_notice(
                        text::tr("Your queue could not be loaded. Saved data has been preserved.")
                            .into(),
                    );
                }
                false
            }
        }
    }

    fn edit_watch_queue(&self, edit: impl FnOnce(&mut Vec<Entry>)) {
        if !self.load_watch_queue() {
            return;
        }
        let mut entries = self
            .shared
            .lock()
            .unwrap()
            .watch_queue
            .entries
            .clone()
            .unwrap_or_default();
        edit(&mut entries);
        let value = serde_json::to_string(&SavedQueue {
            version: 1,
            entries: entries.clone(),
        })
        .expect("queue JSON");
        if let Err(error) = storage::try_set_str(KEY, &value) {
            storage::report(error);
            if let Some(app) = self.app() {
                app.global::<QueueBridge>().set_notice(
                    text::tr("Your queue could not be saved. Please try again.").into(),
                );
            }
            return;
        }
        {
            let mut state = self.shared.lock().unwrap();
            let revision = image_settings_revision();
            let live: HashSet<_> = entries
                .iter()
                .map(|entry| entry.artwork_key(revision))
                .collect();
            state
                .watch_queue
                .artwork_requested
                .retain(|key| live.contains(key));
            state.watch_queue.entries = Some(entries);
        }
        if let Some(app) = self.app() {
            app.global::<QueueBridge>().set_notice("".into());
        }
        self.refresh_watch_queue();
    }

    fn queue_entry(&self, entry: Entry) {
        if !self.load_watch_queue() {
            return;
        }
        let entries = self
            .shared
            .lock()
            .unwrap()
            .watch_queue
            .entries
            .clone()
            .unwrap_or_default();
        let exists = entries.iter().any(|e| e.key() == entry.key());
        if !exists && entries.len() >= MAX_ITEMS {
            if let Some(app) = self.app() {
                app.global::<QueueBridge>().set_notice(
                    text::tr("Your queue is full. Remove an item before adding another.").into(),
                );
            }
            return;
        }
        self.edit_watch_queue(|entries| {
            if !exists {
                entries.push(entry);
            }
        });
    }

    fn entry_from_modal(modal: &ModalItem, video: Option<&Video>) -> Entry {
        Entry {
            type_: modal.type_.clone(),
            series_id: modal.id.clone(),
            episode_id: video.map_or_else(|| modal.id.clone(), |v| v.id.clone()),
            title: modal.name.clone(),
            poster_url: modal.poster_url.clone(),
            episode_title: video.map(Video::label).unwrap_or_default(),
            season: video.and_then(|v| v.season),
            episode: video.and_then(Video::episode_number),
        }
    }

    fn queue_detail(&self) {
        let entry = {
            let state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_ref().filter(|m| m.type_ == "movie") else {
                return;
            };
            Self::entry_from_modal(modal, None)
        };
        self.queue_entry(entry);
    }

    fn queue_episode(&self, index: i32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let Some((_, episode, _)) = self.resolve_filtered_episode(index) else {
            return;
        };
        let entry = {
            let state = self.shared.lock().unwrap();
            let Some(modal) = state.modal_item.as_ref() else {
                return;
            };
            let Some(video) = modal.videos.iter().find(|v| v.id == episode) else {
                return;
            };
            Self::entry_from_modal(modal, Some(video))
        };
        self.queue_entry(entry);
    }

    pub(super) fn refresh_watch_queue(&self) {
        let Some(app) = self.app() else { return };
        let queue = app.global::<QueueBridge>();
        if !self.load_watch_queue() {
            queue.set_rows(Rc::new(VecModel::default()).into());
            queue.set_summary("".into());
            queue.set_detail_queued(false);
            return;
        }
        let (entries, progress, detail_id) = {
            let state = self.shared.lock().unwrap();
            (
                state.watch_queue.entries.clone().unwrap_or_default(),
                state.progress.clone(),
                state
                    .modal_item
                    .as_ref()
                    .filter(|m| m.type_ == "movie")
                    .map(|m| Self::entry_from_modal(m, None).key()),
            )
        };
        queue.set_detail_queued(detail_id.is_some_and(|id| entries.iter().any(|e| e.key() == id)));
        queue.set_summary(
            text::watch_queue_summary(
                entries.iter().filter(|e| e.type_ == "movie").count(),
                entries.iter().filter(|e| e.type_ != "movie").count(),
            )
            .into(),
        );
        let rows: Vec<_> = entries
            .iter()
            .map(|entry| {
                let pixels =
                    decoded_cache_get(&image_cache_key(&entry.poster_url, ImageUse::DetailPoster))
                        .or_else(|| {
                            decoded_cache_get(&image_cache_key(
                                &entry.poster_url,
                                Some(DISPLAY_POSTER_SIDE),
                            ))
                        });
                WatchQueueRow {
                    id: entry.key().into(),
                    title: entry.title.clone().into(),
                    context: entry.context().into(),
                    has_poster: pixels.is_some(),
                    poster: pixels.map(Image::from_rgba8).unwrap_or_default(),
                    watched: progress
                        .get(&progress_map_key(&entry.series_id, &entry.episode_id))
                        .is_some_and(|p| p.watched),
                }
            })
            .collect();
        queue.set_rows(Rc::new(VecModel::from(rows)).into());
        if queue.get_open() {
            self.load_queue_artwork(entries);
        }
    }

    fn load_queue_artwork(&self, entries: Vec<Entry>) {
        let revision = image_settings_revision();
        let pending: Vec<_> = {
            let mut state = self.shared.lock().unwrap();
            let live: HashSet<_> = entries
                .iter()
                .map(|entry| entry.artwork_key(revision))
                .collect();
            state
                .watch_queue
                .artwork_requested
                .retain(|key| live.contains(key));
            entries
                .into_iter()
                .filter(|entry| {
                    !entry.poster_url.is_empty()
                        && !state
                            .watch_queue
                            .artwork_requested
                            .contains(&entry.artwork_key(revision))
                        && decoded_cache_get(&image_cache_key(
                            &entry.poster_url,
                            ImageUse::DetailPoster,
                        ))
                        .is_none()
                })
                .collect()
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
            let worker = ArtworkWorker;
            for entry in pending {
                // Get permission immediately before each request. The worker
                // waits for the UI decision, so navigation stops the batch
                // before another image download starts.
                let (send, receive) = std::sync::mpsc::sync_channel(1);
                let check = b.clone();
                let candidate = entry.clone();
                if profiles::invoke_from_event_loop(move || {
                    let visible = check
                        .app()
                        .is_some_and(|app| app.global::<QueueBridge>().get_open());
                    let mut state = check.shared.lock().unwrap();
                    let current = state.watch_queue.entries.as_ref().is_some_and(|entries| {
                        entries.iter().any(|entry| {
                            entry.key() == candidate.key()
                                && entry.poster_url == candidate.poster_url
                        })
                    });
                    let allowed = visible && current && image_settings_revision() == revision;
                    if allowed {
                        state
                            .watch_queue
                            .artwork_requested
                            .insert(candidate.artwork_key(revision));
                    }
                    let _ = send.send(allowed);
                })
                .is_err()
                    || receive.recv_timeout(Duration::from_secs(2)) != Ok(true)
                {
                    break;
                }
                let pixels = display_pixels(&entry.poster_url, ImageUse::DetailPoster);
                let publish = b.clone();
                let _ = profiles::invoke_from_event_loop(move || {
                    let Some(app) = publish.app() else { return };
                    let queue = app.global::<QueueBridge>();
                    if !queue.get_open() || image_settings_revision() != revision {
                        return;
                    }
                    let current = publish
                        .shared
                        .lock()
                        .unwrap()
                        .watch_queue
                        .entries
                        .as_ref()
                        .is_some_and(|entries| {
                            entries.iter().any(|candidate| {
                                candidate.key() == entry.key()
                                    && candidate.poster_url == entry.poster_url
                            })
                        });
                    if !current {
                        return;
                    }
                    let model = queue.get_rows();
                    let Some(rows) = model.as_any().downcast_ref::<VecModel<WatchQueueRow>>()
                    else {
                        return;
                    };
                    if let Some((index, mut row)) = rows
                        .iter()
                        .enumerate()
                        .find(|(_, row)| row.id == entry.key())
                        && let Some(pixels) = pixels
                    {
                        row.poster = Image::from_rgba8(pixels);
                        row.has_poster = true;
                        rows.set_row_data(index, row);
                    }
                });
            }
            drop(worker);
            // Release the process slot before refreshing the selected profile:
            // it may have gained queue items while this batch was in flight.
            let _ = profiles::invoke_account(move || {
                profiles::with_context(profiles::active_context(), || b.refresh_watch_queue());
            });
        });
    }

    fn play_queued(&self, key: &str) {
        if !self.load_watch_queue() {
            return;
        }
        let entry = self
            .shared
            .lock()
            .unwrap()
            .watch_queue
            .entries
            .as_ref()
            .and_then(|entries| entries.iter().find(|e| e.key() == key))
            .cloned();
        let Some(entry) = entry else { return };
        if let Some(app) = self.app() {
            app.global::<QueueBridge>().set_open(false);
        }
        self.open_preview(
            MetaPreview {
                id: entry.series_id.clone(),
                type_: entry.type_.clone(),
                name: entry.title.clone(),
                poster: (!entry.poster_url.is_empty()).then(|| entry.poster_url.clone()),
                ..Default::default()
            },
            0,
            false,
            None,
            false,
        );
        if entry.type_ == "movie" {
            self.watch_now();
        } else {
            let video = {
                let mut state = self.shared.lock().unwrap();
                let Some(modal) = state
                    .modal_item
                    .as_mut()
                    .filter(|m| m.id == entry.series_id && m.type_ == entry.type_)
                else {
                    return;
                };
                if let Some(video) = modal.videos.iter().find(|v| v.id == entry.episode_id) {
                    video.clone()
                } else {
                    let video = entry.video();
                    modal.videos.push(video.clone());
                    video
                }
            };
            self.select_episode_streams(entry.series_id, entry.episode_id, video, 0);
        }
        if let Some(app) = self.app() {
            app.set_detail_deep_stream(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(id: &str, episode: &str) -> Entry {
        Entry {
            type_: "series".into(),
            series_id: id.into(),
            episode_id: episode.into(),
            title: id.into(),
            poster_url: String::new(),
            episode_title: String::new(),
            season: Some(1),
            episode: Some(1),
        }
    }
    #[test]
    fn queue_roundtrip_deduplicates_exact_typed_identity() {
        let first = entry("a", "e1");
        let second = entry("a", "e2");
        let mut movie = first.clone();
        movie.type_ = "movie".into();
        let value = serde_json::to_string(&SavedQueue {
            version: 1,
            entries: vec![first.clone(), second.clone(), first.clone(), movie.clone()],
        })
        .unwrap();
        assert_eq!(decode(&value).unwrap(), vec![first, second, movie]);
        assert!(decode(r#"{"version":2,"entries":[]}"#).is_err());
        assert!(decode("broken").is_err());
    }
    #[test]
    fn artwork_identity_tracks_source_and_image_settings() {
        let mut first = entry("a", "1");
        first.poster_url = "https://images.example/first".into();
        let original = first.artwork_key(1);
        assert_ne!(original, first.artwork_key(2));
        first.poster_url = "https://images.example/second".into();
        assert_ne!(original, first.artwork_key(1));
    }

    #[test]
    fn malformed_identity_is_rejected_without_discarding_other_saved_entries() {
        let mut invalid = entry("a", "1");
        invalid.episode_id = "  ".into();
        let value = serde_json::to_string(&SavedQueue {
            version: 1,
            entries: vec![entry("valid", "1"), invalid],
        })
        .unwrap();
        assert!(decode(&value).is_err());
        let oversized = serde_json::to_string(&SavedQueue {
            version: 1,
            entries: vec![entry("a", "1"); MAX_ITEMS + 1],
        })
        .unwrap();
        assert!(decode(&oversized).is_err());
    }

    #[test]
    fn reorder_uses_identity_and_stops_at_boundaries() {
        let mut entries = vec![entry("a", "1"), entry("b", "1"), entry("a", "2")];
        let key = entries[1].key();
        let original = entries.clone();
        move_entry(&mut entries, &key, 0);
        move_entry(&mut entries, &key, 2);
        assert_eq!(entries, original);
        move_entry(&mut entries, &key, -1);
        move_entry(&mut entries, &key, -1);
        assert_eq!(entries[0].key(), key);
        move_entry(&mut entries, "stale", 1);
        move_entry(&mut entries, &key, 1);
        move_entry(&mut entries, &key, 1);
        move_entry(&mut entries, &key, 1);
        assert_eq!(entries[2].key(), key);
        assert_ne!(entry("a\u{1}b", "c").key(), entry("a", "b\u{1}c").key());
    }
}

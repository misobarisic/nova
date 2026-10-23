//! A deliberate evening plan, independent of Library membership and playback
//! history. Choosing a source never silently consumes the planned item.
use super::*;
use crate::{QueueBridge, WatchQueueRow};

pub(crate) const KEY: &str = "watch_queue:v2";
pub(crate) const ITEMS: &str = "watch_queue_items";
pub(crate) const ORDER: &str = "watch_queue_order";
pub(crate) const CURSOR: &str = "watch_queue_cursor";
mod model;
use model::SavedQueue;
pub(crate) use model::{portable_merge, records, snapshot};
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
pub(super) struct Entry {
    pub(super) type_: String,
    pub(super) series_id: String,
    pub(super) episode_id: String,
    pub(super) title: String,
    pub(super) poster_url: String,
    pub(super) episode_title: String,
    pub(super) season: Option<u32>,
    pub(super) episode: Option<u32>,
    #[serde(default)]
    pub(super) released: Option<String>,
    #[serde(default)]
    pub(super) created_ms: u64,
    #[serde(default)]
    pub(super) created_device: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) source_preference: Option<StreamPreference>,
    #[serde(flatten)]
    pub(super) extra: std::collections::BTreeMap<String, serde_json::Value>,
}

impl Entry {
    fn matches_playback(
        &self,
        media_type: &str,
        title: &str,
        episode: &str,
        videos: &[Video],
    ) -> bool {
        self.type_ == media_type
            && self.series_id == title
            && (self.episode_id == episode
                || remote_playback::remote_episode(videos, &self.episode_id)
                    .is_some_and(|video| video.id == episode))
    }

    pub(super) fn identity(&self) -> nova_playback_api::MediaIdentity {
        nova_playback_api::MediaIdentity {
            media_type: self.type_.clone(),
            title_id: self.series_id.clone(),
            episode_id: self.episode_id.clone(),
            title: self.title.clone(),
            episode: self.episode_title.clone(),
        }
    }
    pub(super) fn key(&self) -> String {
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

    pub(super) fn video(&self) -> Video {
        Video {
            id: self.episode_id.clone(),
            name: self.episode_title.clone(),
            season: self.season,
            episode: self.episode,
            released: self.released.clone(),
            ..Default::default()
        }
    }
}

#[derive(Default)]
pub(super) struct State {
    pub(super) entries: Option<Vec<Entry>>,
    artwork_requested: HashSet<String>,
    saved: SavedQueue,
    run: Option<Run>,
    sequence: u64,
    transitioning: bool,
}

#[derive(Clone)]
struct Run {
    id: u64,
    current: String,
    order: Vec<String>,
    native: u64,
    eof: Option<u64>,
    paused: bool,
    continuation: bool,
    autoplay: bool,
    skip_watched: bool,
    finished: bool,
}

impl Run {
    fn observe_native(&mut self, native: u64, ended: bool) -> Option<model::Phase> {
        if self.native != native {
            self.native = native;
            self.eof = None;
            Some(model::Phase::InProgress)
        } else if ended && self.eof != Some(native) {
            self.eof = Some(native);
            Some(model::Phase::Completed)
        } else {
            None
        }
    }
    fn successor(&self, entries: &[Entry]) -> Option<Entry> {
        if let Some(index) = entries.iter().position(|e| e.key() == self.current) {
            return entries.get(index + 1).cloned();
        }
        // Remote removal retains the run's original successor context.
        let index = self.order.iter().position(|key| key == &self.current)?;
        self.order[index + 1..]
            .iter()
            .find_map(|key| entries.iter().find(|entry| entry.key() == *key).cloned())
    }
}

impl State {
    pub(super) fn active(&self) -> bool {
        self.run.is_some()
    }
    pub(super) fn status(
        &self,
        profile: &str,
        countdown: u32,
    ) -> Option<nova_playback_api::QueueStatus> {
        let run = self.run.as_ref()?;
        Some(nova_playback_api::QueueStatus {
            profile_id: profile.into(),
            run_id: run.id,
            item_key: run.current.clone(),
            phase: if run.finished {
                "finished"
            } else if run.paused {
                "paused"
            } else if run.native == 0 {
                "ready"
            } else if countdown > 0 {
                "countdown"
            } else {
                "in_progress"
            }
            .into(),
            countdown,
            autoplay: run.autoplay,
        })
    }
}

#[cfg(test)]
fn decode(raw: &str) -> Result<Vec<Entry>, String> {
    Ok(model::decode(raw)?.entries)
}

pub(super) fn ensure_migrated(owner: &profiles::ProfileStore) -> Result<(), String> {
    load(owner).map(|_| ())
}

fn load(owner: &profiles::ProfileStore) -> Result<SavedQueue, String> {
    if let Some(raw) = owner.read(KEY).map_err(|e| e.to_string())? {
        return model::decode(&raw);
    }
    let saved = owner
        .read("watch_queue:v1")
        .map_err(|e| e.to_string())?
        .map(|raw| model::decode(&raw))
        .transpose()?
        .unwrap_or_default();
    // The original bytes remain available until the new snapshot and all
    // three domain baselines commit in a single storage transaction.
    owner
        .persist_snapshot(KEY, &serde_json::to_string(&saved).unwrap(), false)
        .map_err(|e| e.to_string())?;
    Ok(saved)
}

fn move_entry(entries: &mut [Entry], key: &str, direction: i32, allowed: impl Fn(&Entry) -> bool) {
    if direction != -1 && direction != 1 {
        return;
    }
    let visible: Vec<_> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| allowed(entry))
        .map(|(i, _)| i)
        .collect();
    let Some(position) = visible
        .iter()
        .position(|&index| entries[index].key() == key)
    else {
        return;
    };
    let target = if direction < 0 {
        position.checked_sub(1)
    } else {
        position.checked_add(1).filter(|&i| i < visible.len())
    };
    if let Some(target) = target {
        entries.swap(visible[position], visible[target]);
    }
}

fn patch_watched_rows(rows: &slint::ModelRc<WatchQueueRow>, watched: &HashMap<String, bool>) {
    for (index, mut row) in rows.iter().enumerate() {
        if let Some(&value) = watched.get(row.id.as_str())
            && row.watched != value
        {
            row.watched = value;
            rows.set_row_data(index, row);
        }
    }
}

impl Bridge {
    pub(super) fn queue_note_selected_stream(&self, stream: &StreamUi) {
        let identity = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .map(|modal| {
                (
                    modal.type_.clone(),
                    modal.id.clone(),
                    modal.request_id.clone(),
                )
            });
        if let Some((media_type, title, episode)) = identity {
            self.queue_note_stream_for(&media_type, &title, &episode, stream);
        }
    }

    pub(super) fn queue_note_stream_for(
        &self,
        media_type: &str,
        title: &str,
        episode: &str,
        stream: &StreamUi,
    ) {
        if !stream_recommendation_usable(stream) {
            return;
        }
        if self.shared.lock().unwrap().watch_queue.entries.is_none() {
            let owner = profiles::ProfileStore::capture();
            if owner.read(KEY).ok().flatten().is_none()
                && owner.read("watch_queue:v1").ok().flatten().is_none()
            {
                return;
            }
        }
        if !self.load_watch_queue() {
            return;
        }
        let mut saved = self.shared.lock().unwrap().watch_queue.saved.clone();
        let Some(entry) = saved.entries.iter_mut().find(|entry| {
            entry.type_ == media_type && entry.series_id == title && entry.episode_id == episode
        }) else {
            return;
        };
        let preference = StreamPreference::from_row(stream);
        if entry.source_preference.as_ref() != Some(&preference) {
            entry.source_preference = Some(preference);
            self.save_queue_state(saved);
        }
    }

    fn queue_notice(&self, message: &str) {
        if let Some(app) = self.app() {
            app.global::<QueueBridge>()
                .set_notice(text::tr(message).into());
        }
    }

    pub(super) fn queue_note_playback_start(&self) {
        let actual = self.current_playback_identity();
        let (valid, next) = {
            let state = self.shared.lock().unwrap();
            let queue = &state.watch_queue;
            let Some(run) = queue.run.as_ref() else {
                return;
            };
            let valid = actual.as_ref().is_some_and(|actual| {
                let key = serde_json::to_string(&(
                    &actual.media_type,
                    &actual.title_id,
                    &actual.episode_id,
                ))
                .unwrap();
                key == run.current
                    || queue
                        .entries
                        .as_ref()
                        .and_then(|entries| entries.iter().find(|entry| entry.key() == run.current))
                        .is_some_and(|entry| {
                            let videos = state
                                .modal_item
                                .as_ref()
                                .filter(|modal| {
                                    modal.type_ == actual.media_type && modal.id == actual.title_id
                                })
                                .map(|modal| modal.videos.as_slice())
                                .unwrap_or(&state.remote_playback.receiver_videos);
                            entry.matches_playback(
                                &actual.media_type,
                                &actual.title_id,
                                &actual.episode_id,
                                videos,
                            )
                        })
            });
            (
                valid,
                (valid && run.native == 0 && run.continuation)
                    .then(|| (actual.clone().unwrap(), run.id)),
            )
        };
        if !valid {
            self.preserve_queue_on_close(false);
            self.cancel_queue_run();
            return;
        }
        if let Some((identity, run)) = next {
            self.sleep_timer_note_successor(identity, run);
        }
        if let Some(run) = self.shared.lock().unwrap().watch_queue.run.as_mut() {
            run.paused = false;
        }
    }
    pub(super) fn queue_run_entry(&self) -> Option<(u64, Entry)> {
        let state = self.shared.lock().unwrap();
        let queue = &state.watch_queue;
        let run = queue.run.as_ref()?;
        Some((
            run.id,
            queue
                .entries
                .as_ref()?
                .iter()
                .find(|e| e.key() == run.current)?
                .clone(),
        ))
    }
    pub(super) fn finish_queue(&self) {
        self.queue_cursor(None, model::Phase::Finished, None);
        if let Some(run) = self.shared.lock().unwrap().watch_queue.run.as_mut() {
            run.paused = true;
            run.finished = true;
        }
    }

    pub(super) fn open_queue_successor(&self, entry: &Entry, row: StreamUi) {
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
        self.clear_streams();
        let Some(app) = self.app() else {
            return;
        };
        {
            let mut state = self.shared.lock().unwrap();
            let Some(modal) = state
                .modal_item
                .as_mut()
                .filter(|m| m.id == entry.series_id && m.type_ == entry.type_)
            else {
                drop(state);
                self.pause_queue();
                return;
            };
            modal.request_id = entry.episode_id.clone();
            let force_start_over = state
                .progress
                .get(&progress_map_key(&entry.series_id, &entry.episode_id))
                .is_none_or(|progress| progress.watched || progress.position_secs <= 0.0);
            state.playback = Some(PlaybackTarget {
                series_id: entry.series_id.clone(),
                episode_id: entry.episode_id.clone(),
                force_start_over,
                ..Default::default()
            });
            state.stream_all = vec![row.clone()];
            state.streams = vec![row];
            state.stream_filter = 0;
        }
        apply_stream_episode_context(
            &app,
            (entry.type_ != "movie").then(|| entry.video()).as_ref(),
        );
        self.stream_picked(0);
        self.shared.lock().unwrap().watch_queue.transitioning = false;
    }
    pub(super) fn start_queue(&self, key: Option<&str>) -> Result<(), String> {
        self.start_queue_run(key, false)
    }
    pub(super) fn continue_queue_from(&self, key: &str) -> Result<(), String> {
        self.start_queue_run(Some(key), true)
    }

    fn start_queue_run(&self, key: Option<&str>, continuation: bool) -> Result<(), String> {
        if profile_policy::entry_blocked() {
            return Err("Select and unlock the same profile on the playing device.".into());
        }
        if self.remote_output_selected() {
            self.remote_start_queue(key);
            return Ok(());
        }
        if self.party_active()
            || self
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .uses_external_player()
        {
            return Err("Queue playback is unavailable".into());
        }
        if !self.load_watch_queue() {
            return Err("Queue is unavailable".into());
        }
        let selected = {
            let state = self.shared.lock().unwrap();
            let saved = &state.watch_queue.saved;
            if key.is_none() && saved.cursor.phase == model::Phase::Finished {
                None
            } else if let Some(key) = key {
                saved.entries.iter().find(|e| e.key() == key).cloned()
            } else {
                let cursor = &saved.cursor;
                let start = cursor
                    .item_key
                    .as_ref()
                    .and_then(|key| saved.entries.iter().position(|e| e.key() == *key))
                    .unwrap_or(0);
                saved
                    .entries
                    .iter()
                    .skip(start)
                    .find(|entry| {
                        if !profile_policy::allows_title(
                            &profiles::ProfileStore::capture(),
                            &entry.type_,
                            &entry.series_id,
                        ) {
                            return false;
                        }
                        let progress = state
                            .progress
                            .get(&progress_map_key(&entry.series_id, &entry.episode_id));
                        !progress.is_some_and(|p| p.watched)
                            || (cursor.phase == model::Phase::InProgress
                                && cursor.item_key.as_ref() == Some(&entry.key())
                                && progress.is_some_and(|p| p.position_secs > 0.0))
                    })
                    .cloned()
            }
        };
        let Some(entry) = selected else {
            self.queue_cursor(None, model::Phase::Finished, None);
            return Err("No pending queue item".into());
        };
        if !profile_policy::allows_title(
            &profiles::ProfileStore::capture(),
            &entry.type_,
            &entry.series_id,
        ) {
            return Err("Title is unavailable".into());
        }
        self.clear_playback_end_menu();
        {
            let mut state = self.shared.lock().unwrap();
            let queue = &mut state.watch_queue;
            let previous = continuation.then(|| queue.run.clone()).flatten();
            queue.sequence = queue.sequence.wrapping_add(1).max(1);
            queue.run = Some(Run {
                id: previous.as_ref().map_or(queue.sequence, |run| run.id),
                current: entry.key(),
                order: queue
                    .entries
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(Entry::key)
                    .collect(),
                native: 0,
                eof: None,
                paused: false,
                continuation,
                autoplay: previous
                    .as_ref()
                    .map_or(queue.saved.autoplay, |run| run.autoplay),
                skip_watched: previous
                    .as_ref()
                    .map_or(key.is_none(), |run| run.skip_watched),
                finished: false,
            });
        }
        self.open_queued_source(&entry.key());
        self.refresh_watch_queue();
        Ok(())
    }

    fn save_queue_state(&self, saved: SavedQueue) -> bool {
        if profile_policy::entry_blocked() {
            return false;
        }
        let raw = serde_json::to_string(&saved).expect("queue JSON");
        if let Err(error) = sync::persist_sync_snapshot(KEY, &raw, false) {
            storage::report(storage::Error::new(storage::ErrorKind::Transaction, error));
            self.queue_notice("Your queue could not be saved. Please try again.");
            return false;
        }
        let mut state = self.shared.lock().unwrap();
        state.watch_queue.entries = Some(saved.entries.clone());
        state.watch_queue.saved = saved;
        drop(state);
        self.refresh_watch_queue();
        true
    }

    fn queue_cursor(
        &self,
        key: Option<String>,
        phase: model::Phase,
        completed: Option<String>,
    ) -> bool {
        let mut saved = self.shared.lock().unwrap().watch_queue.saved.clone();
        saved.cursor.item_key = key;
        saved.cursor.phase = phase;
        if completed.is_some() {
            saved.cursor.last_completed_key = completed;
        }
        saved.cursor.origin_device = nova_sync::local_device().unwrap_or_default();
        model::materialize(&mut saved);
        self.save_queue_state(saved)
    }

    pub(super) fn preserve_queue_on_close(&self, preserve: bool) {
        self.shared.lock().unwrap().watch_queue.transitioning = preserve;
    }

    pub(super) fn stop_queue(&self) {
        if self.remote_output_selected() {
            self.remote_queue_action(nova_playback_api::PlaybackAction::QueueStop);
            return;
        }
        self.cancel_queue_run();
        let _ = self.execute_playback_action(nova_playback_api::PlaybackAction::Stop);
    }

    pub(super) fn cancel_queue_run(&self) {
        let mut state = self.shared.lock().unwrap();
        if state.watch_queue.transitioning {
            return;
        }
        state.watch_queue.run = None;
        drop(state);
        self.clear_playback_end_menu();
        if let Some(app) = self.app() {
            app.global::<QueueBridge>().set_run_active(false);
        }
    }

    pub(super) fn pause_queue(&self) {
        let (native, already_paused) = self
            .shared
            .lock()
            .unwrap()
            .watch_queue
            .run
            .as_ref()
            .map(|r| (r.native, r.paused))
            .unwrap_or((0, true));
        if already_paused {
            return;
        }
        let pending = if native != 0 {
            self.queue_successor()
        } else {
            self.queue_run_entry().map(|(_, e)| e)
        };
        let Some(entry) = pending else {
            self.finish_queue();
            return;
        };
        let run_id = {
            let mut state = self.shared.lock().unwrap();
            let Some(run) = state.watch_queue.run.as_mut() else {
                return;
            };
            run.current = entry.key();
            run.native = 0;
            run.eof = None;
            run.continuation |= native != 0;
            run.paused = true;
            run.id
        };
        if let Some(app) = self.app().filter(|app| app.get_player_open()) {
            self.sleep_timer_note_successor(entry.identity(), run_id);
            self.preserve_queue_on_close(true);
            self.player
                .with_preserved_fullscreen_on_close(|| app.invoke_close_player());
            self.preserve_queue_on_close(false);
            self.sleep_timer_finish_successor_close();
        }
        if native != 0 {
            self.queue_cursor(Some(entry.key()), model::Phase::Ready, None);
        }
        self.open_queued_source(&entry.key());
        if let Some(app) = self.app() {
            app.global::<QueueBridge>().set_notice(
                text::tr("Queue paused. Choose a source, retry, skip or stop the queue.").into(),
            );
        }
    }

    pub(super) fn queue_successor(&self) -> Option<Entry> {
        let state = self.shared.lock().unwrap();
        let queue = &state.watch_queue;
        let mut run = queue.run.clone()?;
        let entries = queue.entries.as_ref()?;
        for _ in 0..entries.len() {
            let next = run.successor(entries)?;
            if profile_policy::allows_title(
                &profiles::ProfileStore::capture(),
                &next.type_,
                &next.series_id,
            ) && (!run.skip_watched
                || !state
                    .progress
                    .get(&progress_map_key(&next.series_id, &next.episode_id))
                    .is_some_and(|p| p.watched))
            {
                return Some(next);
            }
            run.current = next.key();
        }
        None
    }

    pub(super) fn queue_run_active(&self) -> bool {
        self.shared.lock().unwrap().watch_queue.run.is_some()
    }
    pub(super) fn queue_autoplay(&self) -> bool {
        let state = self.shared.lock().unwrap();
        state
            .watch_queue
            .run
            .as_ref()
            .is_some_and(|r| r.autoplay && !r.paused)
    }

    pub(super) fn queue_accept_successor(&self, entry: &Entry) -> bool {
        if !profile_policy::allows_title(
            &profiles::ProfileStore::capture(),
            &entry.type_,
            &entry.series_id,
        ) {
            self.pause_queue();
            return false;
        }
        let identity = entry.identity();
        if self.sleep_timer_blocks_successor(&identity) {
            return false;
        }
        let run_id = {
            let mut state = self.shared.lock().unwrap();
            let queue = &mut state.watch_queue;
            let Some(run) = queue.run.as_mut().filter(|r| !r.paused) else {
                return false;
            };
            run.current = entry.key();
            run.native = 0;
            run.eof = None;
            run.continuation = true;
            run.order = queue
                .entries
                .as_ref()
                .unwrap_or(&Vec::new())
                .iter()
                .map(Entry::key)
                .collect();
            let id = run.id;
            queue.transitioning = true;
            id
        };
        self.sleep_timer_note_successor(identity, run_id);
        true
    }

    pub(super) fn queue_skip_cursor(&self, key: &str) {
        self.queue_cursor(Some(key.into()), model::Phase::Ready, None);
    }

    pub(super) fn skip_queue(&self) {
        if self.remote_output_selected() {
            self.remote_queue_action(nova_playback_api::PlaybackAction::QueueSkip);
            return;
        }
        let next = self.queue_successor();
        if let Some(run) = self.shared.lock().unwrap().watch_queue.run.as_mut() {
            run.paused = false;
        }
        self.clear_playback_end_menu();
        if let Some(entry) = next {
            if !profile_policy::allows_title(
                &profiles::ProfileStore::capture(),
                &entry.type_,
                &entry.series_id,
            ) || self.sleep_timer_blocks_successor(&entry.identity())
            {
                return;
            }
            {
                let mut state = self.shared.lock().unwrap();
                if let Some(run) = state.watch_queue.run.as_mut() {
                    run.current = entry.key();
                    run.native = 0;
                    run.eof = None;
                    run.continuation = true;
                }
            }
            self.queue_cursor(Some(entry.key()), model::Phase::Ready, None);
            if let Some(app) = self.app().filter(|app| app.get_player_open()) {
                let run_id = self.queue_run_entry().map_or(0, |(id, _)| id);
                self.sleep_timer_note_successor(entry.identity(), run_id);
                self.preserve_queue_on_close(true);
                self.player
                    .with_preserved_fullscreen_on_close(|| app.invoke_close_player());
                self.preserve_queue_on_close(false);
                self.sleep_timer_finish_successor_close();
            }
            self.open_queued_source(&entry.key());
        } else {
            self.queue_cursor(None, model::Phase::Finished, None);
            self.pause_queue();
        }
    }

    /// Observe accepted native loads and natural EOF only. Merely opening a
    /// picker never mutates the durable continuation cursor.
    pub(super) fn refresh_queue_playback(&self, ended: bool) {
        let Some(app) = self.app() else { return };
        if self.player.playback_failed() && self.queue_run_active() {
            if let Some(run) = self
                .shared
                .lock()
                .unwrap()
                .watch_queue
                .run
                .as_mut()
                .filter(|run| !run.paused)
            {
                run.native = 0;
            }
            self.pause_queue();
            return;
        }
        if !app.get_player_open() || !app.get_playback_started() {
            return;
        }
        self.observe_queue_playback(self.player.screenshot_session(), ended);
    }

    /// Receivers supply their own accepted-load identity and explicit finished reason.
    pub(super) fn observe_queue_playback(&self, native: u64, ended: bool) {
        let mutation = {
            let mut state = self.shared.lock().unwrap();
            let identity = state.playback.as_ref().and_then(|p| {
                p.history.as_ref().map(|h| {
                    let queued = state.watch_queue.run.as_ref().and_then(|run| {
                        state
                            .watch_queue
                            .entries
                            .as_ref()?
                            .iter()
                            .find(|entry| entry.key() == run.current)
                    });
                    let videos = state
                        .modal_item
                        .as_ref()
                        .filter(|m| m.type_ == h.type_ && m.id == p.series_id)
                        .map(|m| m.videos.as_slice())
                        .unwrap_or(&state.remote_playback.receiver_videos);
                    if let Some(entry) = queued.filter(|entry| {
                        entry.matches_playback(&h.type_, &p.series_id, &p.episode_id, videos)
                    }) {
                        entry.key()
                    } else {
                        serde_json::to_string(&(&h.type_, &p.series_id, &p.episode_id)).unwrap()
                    }
                })
            });
            let queue = &mut state.watch_queue;
            let Some(run) = queue.run.as_mut() else {
                return;
            };
            if identity.as_ref() != Some(&run.current) {
                // The previous player may still be visible during first-item
                // source selection; only a replacement of an accepted load
                // cancels an already running queue.
                if run.native != 0 {
                    queue.run = None;
                }
                return;
            }
            run.observe_native(native, ended).map(|phase| {
                (
                    run.current.clone(),
                    phase,
                    (phase == model::Phase::Completed).then(|| run.current.clone()),
                )
            })
        };
        if let Some((key, phase, completed)) = mutation {
            if !self.queue_cursor(Some(key), phase, completed) {
                if let Some(run) = self.shared.lock().unwrap().watch_queue.run.as_mut() {
                    run.native = 0;
                    run.eof = None;
                }
                return;
            }
            if phase == model::Phase::Completed && self.queue_successor().is_none() {
                self.finish_queue();
            }
        }
    }

    /// A watch edit only changes flags in an already loaded queue. Keep its
    /// artwork and rows intact without loading storage or cloning all history.
    pub(super) fn refresh_watch_queue_progress(&self) {
        let Some(app) = self.app() else { return };
        let rows = app.global::<QueueBridge>().get_rows();
        if rows.row_count() == 0 {
            return;
        }
        let watched = {
            let state = self.shared.lock().unwrap();
            let Some(entries) = state.watch_queue.entries.as_ref() else {
                return;
            };
            entries
                .iter()
                .map(|entry| {
                    let value = state
                        .progress
                        .get(&progress_map_key(&entry.series_id, &entry.episode_id))
                        .is_some_and(|progress| progress.watched);
                    (entry.key(), value)
                })
                .collect()
        };
        // Model notifications can evaluate bindings; Shared is released first.
        patch_watched_rows(&rows, &watched);
    }

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
        queue.on_continue_queue(move || {
            if let Err(error) = b.start_queue(None) {
                b.queue_notice(&error);
            }
        });
        let b = self.clone();
        queue.on_start_from(move |key| {
            if let Err(error) = b.start_queue(Some(&key)) {
                b.queue_notice(&error);
            }
        });
        let b = self.clone();
        queue.on_skip(move || b.skip_queue());
        let b = self.clone();
        queue.on_stop_queue(move || b.stop_queue());
        let b = self.clone();
        queue.on_set_autoplay(move |value| {
            if !b.load_watch_queue() {
                return;
            }
            let mut saved = b.shared.lock().unwrap().watch_queue.saved.clone();
            saved.autoplay = value;
            b.save_queue_state(saved);
            if !value && let Some(run) = b.shared.lock().unwrap().watch_queue.run.as_mut() {
                run.autoplay = false;
            }
            b.clear_playback_end_menu();
        });
        let b = self.clone();
        queue.on_move(move |key, direction| {
            let owner = profiles::ProfileStore::capture();
            b.edit_watch_queue(|entries| {
                move_entry(entries, &key, direction, |entry| {
                    profile_policy::allows_title(&owner, &entry.type_, &entry.series_id)
                })
            });
        });
        let b = self.clone();
        queue.on_remove(move |key| {
            b.edit_watch_queue(|entries| {
                entries.retain(|entry| {
                    entry.key() != key.as_str()
                        || !profile_policy::allows_title(
                            &profiles::ProfileStore::capture(),
                            &entry.type_,
                            &entry.series_id,
                        )
                })
            });
        });
        let b = self.clone();
        queue.on_clear_watched(move || {
            let watched = {
                let state = b.shared.lock().unwrap();
                state.progress.clone()
            };
            b.edit_watch_queue(|entries| {
                entries.retain(|entry| {
                    !profile_policy::allows_title(
                        &profiles::ProfileStore::capture(),
                        &entry.type_,
                        &entry.series_id,
                    ) || !watched
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
        let result = load(&profiles::ProfileStore::capture());
        match result {
            Ok(saved) => {
                let mut state = self.shared.lock().unwrap();
                state.watch_queue.entries = Some(saved.entries.clone());
                state.watch_queue.saved = saved;
                drop(state);
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
        if profile_policy::entry_blocked() {
            return;
        }
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
        let previous_order: Vec<_> = entries.iter().map(Entry::key).collect();
        edit(&mut entries);
        let mut saved = self.shared.lock().unwrap().watch_queue.saved.clone();
        saved.entries = entries.clone();
        let next_order: Vec<_> = entries.iter().map(Entry::key).collect();
        if previous_order != next_order {
            saved.order = next_order;
        }
        let value = serde_json::to_string(&saved).expect("queue JSON");
        if let Err(error) = sync::persist_sync_snapshot(KEY, &value, false) {
            storage::report(storage::Error::new(storage::ErrorKind::Transaction, error));
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
            state.watch_queue.saved = saved;
        }
        if let Some(app) = self.app() {
            app.global::<QueueBridge>().set_notice("".into());
        }
        self.refresh_watch_queue();
    }

    fn queue_entry(&self, mut entry: Entry) {
        if profile_policy::entry_blocked() {
            return;
        }
        if !profile_policy::allows_title(
            &profiles::ProfileStore::capture(),
            &entry.type_,
            &entry.series_id,
        ) {
            return;
        }
        entry.created_ms = nova_config::now_ms();
        entry.created_device = nova_sync::local_device().unwrap_or_default();
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
            released: video.and_then(|v| v.released.clone()),
            created_ms: 0,
            created_device: 0,
            source_preference: None,
            extra: Default::default(),
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
        if profile_policy::entry_blocked() {
            return;
        }
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
                state
                    .watch_queue
                    .entries
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|entry| {
                        profile_policy::allows_title(
                            &profiles::ProfileStore::capture(),
                            &entry.type_,
                            &entry.series_id,
                        )
                    })
                    .collect::<Vec<_>>(),
                state.progress.clone(),
                state
                    .modal_item
                    .as_ref()
                    .filter(|m| m.type_ == "movie")
                    .map(|m| Self::entry_from_modal(m, None).key()),
            )
        };
        {
            let state = self.shared.lock().unwrap();
            queue.set_autoplay(state.watch_queue.saved.autoplay);
            queue.set_run_active(state.watch_queue.run.is_some());
            queue.set_current_key(
                state
                    .watch_queue
                    .saved
                    .cursor
                    .item_key
                    .clone()
                    .unwrap_or_default()
                    .into(),
            );
            queue.set_phase(
                match state.watch_queue.saved.cursor.phase {
                    model::Phase::Ready => text::tr("Ready"),
                    model::Phase::InProgress => text::tr("In progress"),
                    model::Phase::Completed => text::tr("Completed"),
                    model::Phase::Finished => text::tr("Finished"),
                }
                .into(),
            );
        }
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
        let focused_key = queue.get_focused_key().to_string();
        let focused_index = rows
            .iter()
            .position(|row| row.id == focused_key)
            .map(|i| i as i32)
            .unwrap_or_else(|| queue.get_focused_index().clamp(-1, rows.len() as i32 - 1));
        queue.set_rows(Rc::new(VecModel::from(rows)).into());
        queue.set_focused_index(focused_index);
        queue.set_sync_status(app.get_sync_status());
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
        self.cancel_queue_run();
        self.open_queued_source(key);
    }

    fn open_queued_source(&self, key: &str) {
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
        if !profile_policy::allows_title(
            &profiles::ProfileStore::capture(),
            &entry.type_,
            &entry.series_id,
        ) {
            return;
        }
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

    fn run(entry: &Entry, order: &[Entry]) -> Run {
        Run {
            id: 1,
            current: entry.key(),
            order: order.iter().map(Entry::key).collect(),
            native: 0,
            eof: None,
            paused: false,
            continuation: false,
            autoplay: true,
            skip_watched: false,
            finished: false,
        }
    }

    #[test]
    fn native_load_and_eof_mutate_cursor_once_and_source_recovery_gets_a_new_load() {
        let first = entry("show", "one");
        let mut run = run(&first, std::slice::from_ref(&first));
        assert_eq!(run.observe_native(7, false), Some(model::Phase::InProgress));
        assert_eq!(run.observe_native(7, false), None);
        assert_eq!(run.observe_native(7, true), Some(model::Phase::Completed));
        assert_eq!(run.observe_native(7, true), None);
        assert_eq!(
            run.observe_native(7, false),
            None,
            "seeking does not invent an accepted start"
        );
        assert_eq!(
            run.observe_native(7, true),
            None,
            "repeated EOF of the same native load is idempotent"
        );
        assert_eq!(run.observe_native(8, false), Some(model::Phase::InProgress));
        assert_eq!(run.observe_native(8, true), Some(model::Phase::Completed));
    }

    #[test]
    fn receiver_confirmed_episode_alias_keeps_the_typed_queue_identity() {
        let queued = entry("show", "provider-id");
        let video = Video {
            id: "receiver-native".into(),
            extra: HashMap::from([("novaStreamIds".into(), serde_json::json!(["provider-id"]))]),
            ..Default::default()
        };
        assert!(queued.matches_playback(
            "series",
            "show",
            "receiver-native",
            std::slice::from_ref(&video)
        ));
        assert!(!queued.matches_playback(
            "movie",
            "show",
            "receiver-native",
            std::slice::from_ref(&video)
        ));
        assert!(!queued.matches_playback(
            "series",
            "other-show",
            "receiver-native",
            std::slice::from_ref(&video)
        ));
        let mut ambiguous = video.clone();
        ambiguous.id = "another-native".into();
        assert!(!queued.matches_playback("series", "show", "receiver-native", &[video, ambiguous]));
    }

    #[test]
    fn successor_obeys_latest_order_and_removed_current_keeps_original_context() {
        let a = entry("show", "a");
        let b = entry("show", "b");
        let c = entry("other", "c");
        let run = run(&a, &[a.clone(), b.clone(), c.clone()]);
        assert_eq!(
            run.successor(&[a.clone(), c.clone(), b.clone()]),
            Some(c.clone())
        );
        assert_eq!(run.successor(&[c.clone(), b.clone()]), Some(b));
        assert_eq!(run.successor(&[c.clone()]), Some(c));
        assert_eq!(run.successor(&[]), None);
        let mut unknown = run;
        unknown.order.clear();
        assert_eq!(
            unknown.successor(&[entry("show", "unrelated")]),
            None,
            "missing context never jumps to an arbitrary row"
        );
    }

    #[test]
    fn synced_autoplay_and_cursor_never_change_a_running_execution() {
        let first = entry("show", "one");
        let mut state = State {
            run: Some(run(&first, std::slice::from_ref(&first))),
            ..Default::default()
        };
        state.saved.autoplay = false;
        state.saved.cursor.item_key = Some("a remote current item".into());
        state.saved.cursor.phase = model::Phase::Finished;
        let snapshot = state.status("profile", 0).unwrap();
        assert_eq!(snapshot.item_key, first.key());
        assert_ne!(
            snapshot.phase, "finished",
            "synced cursor does not retarget local execution status"
        );
        assert!(snapshot.autoplay, "run captures the explicit start choice");
        state.run = None;
        assert!(
            state.status("profile", 0).is_none(),
            "received cursor never creates a player owner"
        );
    }

    #[test]
    fn progress_edits_keep_queue_artwork_context_and_other_episodes() {
        let first = entry("series", "e1");
        let second = entry("series", "e2");
        let poster = Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::new(2, 3));
        let rows: slint::ModelRc<WatchQueueRow> = Rc::new(VecModel::from(vec![
            WatchQueueRow {
                id: first.key().into(),
                title: "Tonight".into(),
                context: "S1 E1 · Opening".into(),
                poster: poster.clone(),
                has_poster: true,
                ..Default::default()
            },
            WatchQueueRow {
                id: second.key().into(),
                context: "S1 E2 · Next".into(),
                ..Default::default()
            },
        ]))
        .into();

        patch_watched_rows(&rows, &HashMap::from([(first.key(), true)]));
        let row = rows.row_data(0).unwrap();
        assert!(row.watched);
        assert_eq!(row.poster, poster);
        assert!(row.has_poster);
        assert_eq!(row.title, "Tonight");
        assert_eq!(row.context, "S1 E1 · Opening");
        assert!(!rows.row_data(1).unwrap().watched);
        assert_eq!(rows.row_data(1).unwrap().context, "S1 E2 · Next");

        patch_watched_rows(&rows, &HashMap::from([(first.key(), false)]));
        assert!(!rows.row_data(0).unwrap().watched);
        assert_eq!(rows.row_data(0).unwrap().poster, poster);
    }

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
            released: None,
            created_ms: 0,
            created_device: 0,
            source_preference: None,
            extra: Default::default(),
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
            ..Default::default()
        })
        .unwrap();
        assert_eq!(decode(&value).unwrap(), vec![first, second, movie]);
        assert!(decode(r#"{"version":3,"entries":[]}"#).is_err());
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
            ..Default::default()
        })
        .unwrap();
        assert!(decode(&value).is_err());
        let oversized = serde_json::to_string(&SavedQueue {
            version: 1,
            entries: vec![entry("a", "1"); MAX_ITEMS + 1],
            ..Default::default()
        })
        .unwrap();
        assert!(decode(&oversized).is_err());
    }

    #[test]
    fn reorder_uses_identity_and_stops_at_boundaries() {
        let mut entries = vec![entry("a", "1"), entry("b", "1"), entry("a", "2")];
        let key = entries[1].key();
        let original = entries.clone();
        move_entry(&mut entries, &key, 0, |_| true);
        move_entry(&mut entries, &key, 2, |_| true);
        assert_eq!(entries, original);
        move_entry(&mut entries, &key, -1, |_| true);
        move_entry(&mut entries, &key, -1, |_| true);
        assert_eq!(entries[0].key(), key);
        move_entry(&mut entries, "stale", 1, |_| true);
        move_entry(&mut entries, &key, 1, |_| true);
        move_entry(&mut entries, &key, 1, |_| true);
        move_entry(&mut entries, &key, 1, |_| true);
        assert_eq!(entries[2].key(), key);
        assert_ne!(entry("a\u{1}b", "c").key(), entry("a", "b\u{1}c").key());
    }

    #[test]
    fn restricted_reorder_moves_visible_rows_without_touching_hidden_membership() {
        let hidden = entry("hidden", "one");
        let first = entry("allowed", "one");
        let second = entry("allowed", "two");
        let mut entries = vec![
            hidden.clone(),
            first.clone(),
            entry("hidden", "two"),
            second.clone(),
        ];
        move_entry(&mut entries, &second.key(), -1, |e| {
            e.series_id == "allowed"
        });
        assert_eq!(entries[1], second);
        assert_eq!(entries[3], first);
        assert_eq!(entries[0], hidden);
        let saved = entries.clone();
        move_entry(&mut entries, &hidden.key(), 1, |e| e.series_id == "allowed");
        assert_eq!(
            entries, saved,
            "a stale hidden row action cannot mutate the plan"
        );
    }
}

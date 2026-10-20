//! Profile-local scene notes and a guarded handoff to the next offered source.
use super::*;
use nova_config::playback_preferences::TrackChoice;

const BOOKMARKS_KEY: &str = "scene_bookmarks:v1";
const SECONDARY_KEY: &str = "secondary_subtitles:v1";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Bookmark {
    position: f64,
    note: String,
}

type Bookmarks = HashMap<String, Vec<Bookmark>>;

#[derive(Default)]
pub(super) struct PlaybackToolsState {
    identity: Option<String>,
    bookmarks: Vec<Bookmark>,
    pub(super) failed_retry: Option<playback::RetryContext>,
    pub(super) pending_retry: Option<playback::RetryContext>,
}

fn identity_key(identity: &nova_playback_api::MediaIdentity) -> String {
    // Length-prefixing through JSON avoids collisions between provider IDs.
    serde_json::to_string(&(
        &identity.media_type,
        &identity.title_id,
        &identity.episode_id,
    ))
    .expect("string tuple is serializable")
}

fn title_key(media_type: &str, title_id: &str) -> String {
    serde_json::to_string(&(media_type, title_id)).expect("string tuple is serializable")
}

impl Bridge {
    pub(super) fn refresh_playback_tools(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let identity = app
            .get_player_open()
            .then(|| self.current_playback_identity())
            .flatten();
        let key = identity.as_ref().map(identity_key);
        let changed = self.shared.lock().unwrap().playback_tools.identity != key;
        if changed {
            let bookmarks = key
                .as_ref()
                .and_then(|key| read_json::<Bookmarks>(BOOKMARKS_KEY)?.remove(key))
                .unwrap_or_default()
                .into_iter()
                .filter(|bookmark| bookmark.position.is_finite() && bookmark.position >= 0.0)
                .take(200)
                .collect::<Vec<_>>();
            let mut state = self.shared.lock().unwrap();
            state.playback_tools.identity = key;
            state.playback_tools.bookmarks = bookmarks;
            drop(state);
            self.publish_scene_bookmarks();
        }
        app.set_scene_bookmarks_available(identity.is_some() && app.get_playback_started());
        app.set_try_another_stream_available(self.alternate_stream_index().is_some());
    }

    fn publish_scene_bookmarks(&self) {
        let rows = self
            .shared
            .lock()
            .unwrap()
            .playback_tools
            .bookmarks
            .iter()
            .map(|bookmark| nova_ui::ChapterRow {
                position: bookmark.position as f32,
                title: bookmark.note.clone().into(),
            })
            .collect::<Vec<_>>();
        if let Some(app) = self.app() {
            app.set_scene_bookmarks(Rc::new(VecModel::from(rows)).into());
        }
    }

    pub(super) fn add_scene_bookmark(&self, note: &str) {
        let Some(app) = self
            .app()
            .filter(|app| app.get_player_open() && app.get_playback_started())
        else {
            return;
        };
        let Some(identity) = self.current_playback_identity() else {
            return;
        };
        let position = app.get_position() as f64;
        if !position.is_finite() || position < 0.0 || !writable_key(BOOKMARKS_KEY) {
            return;
        }
        let key = identity_key(&identity);
        let mut values = read_json::<Bookmarks>(BOOKMARKS_KEY).unwrap_or_default();
        let rows = values.entry(key.clone()).or_default();
        let note: String = note
            .trim()
            .chars()
            .filter(|character| !character.is_control())
            .take(280)
            .collect();
        if let Some(existing) = rows
            .iter_mut()
            .find(|bookmark| (bookmark.position - position).abs() < 0.5)
        {
            if !note.is_empty() {
                existing.note = note;
            }
        } else if rows.len() < 200 {
            rows.push(Bookmark { position, note });
        } else {
            return;
        }
        rows.sort_by(|a, b| a.position.total_cmp(&b.position));
        let bookmarks = rows.clone();
        let failures = write_failures();
        write_json(BOOKMARKS_KEY, &values);
        if failures != write_failures() {
            return;
        }
        let mut state = self.shared.lock().unwrap();
        state.playback_tools.identity = Some(key);
        state.playback_tools.bookmarks = bookmarks;
        drop(state);
        self.publish_scene_bookmarks();
    }

    pub(super) fn delete_scene_bookmark(&self, index: i32) {
        if index < 0 || !writable_key(BOOKMARKS_KEY) {
            return;
        }
        let Some(identity) = self.current_playback_identity() else {
            return;
        };
        let key = identity_key(&identity);
        let selected = {
            let state = self.shared.lock().unwrap();
            if state.playback_tools.identity.as_ref() != Some(&key) {
                return;
            }
            state.playback_tools.bookmarks.get(index as usize).cloned()
        };
        let Some(selected) = selected else {
            return;
        };
        let mut values = read_json::<Bookmarks>(BOOKMARKS_KEY).unwrap_or_default();
        let Some(rows) = values.get_mut(&key) else {
            return;
        };
        rows.retain(|bookmark| bookmark.position != selected.position);
        let bookmarks = rows.clone();
        if rows.is_empty() {
            values.remove(&key);
        }
        let failures = write_failures();
        write_json(BOOKMARKS_KEY, &values);
        if failures != write_failures() {
            return;
        }
        self.shared.lock().unwrap().playback_tools.bookmarks = bookmarks;
        self.publish_scene_bookmarks();
    }

    pub(super) fn prepare_secondary_subtitles(&self) {
        let choice = self
            .playback_preference_identity(true)
            .and_then(|(media_type, id)| {
                read_json::<HashMap<String, TrackChoice>>(SECONDARY_KEY)?
                    .remove(&title_key(&media_type, &id))
            })
            .unwrap_or(TrackChoice::Off);
        self.player.set_secondary_subtitle_preference(choice);
    }

    pub(super) fn pick_secondary_playback_track(&self, id: i32) {
        if !self
            .shared
            .lock()
            .unwrap()
            .remote_playback
            .can_play_locally()
        {
            return;
        }
        let Some((media_type, title)) = self.playback_preference_identity(false) else {
            return;
        };
        self.cancel_secondary_subtitle_download();
        if id < -1 {
            self.load_secondary_addon_subtitle(id, true);
            return;
        }
        if let Some(choice) = self.player.pick_secondary_subtitle(id) {
            let mut values =
                read_json::<HashMap<String, TrackChoice>>(SECONDARY_KEY).unwrap_or_default();
            values.insert(title_key(&media_type, &title), choice);
            write_json(SECONDARY_KEY, &values);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bookmark_identity_keeps_title_episode_and_media_type_distinct() {
        let identity = nova_playback_api::MediaIdentity {
            media_type: "series".into(),
            title_id: "a:b".into(),
            episode_id: "c".into(),
            title: String::new(),
            episode: String::new(),
        };
        let mut other = identity.clone();
        other.title_id = "a".into();
        other.episode_id = "b:c".into();
        assert_ne!(identity_key(&identity), identity_key(&other));
        other = identity.clone();
        other.media_type = "movie".into();
        assert_ne!(identity_key(&identity), identity_key(&other));
        other = identity.clone();
        other.title = "translated caption".into();
        assert_eq!(identity_key(&identity), identity_key(&other));
    }
}

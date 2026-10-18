//! Subtitle addons never gate the video connection. Results and downloads are
//! bound to the native playback session and the viewer's latest selection.
use super::*;
use crate::player::OnlineSubtitle;

#[derive(Default)]
pub(super) struct State {
    session: u64,
    selection: u64,
    pending: usize,
    failed: usize,
    pub(super) automatic_attempted: bool,
    request: Option<Lookup>,
    owners: HashMap<String, (String, u64)>,
    choices: Vec<OnlineSubtitle>,
    files: Vec<SubtitleFile>,
}

#[derive(Clone)]
struct Lookup {
    media_type: String,
    id: String,
    hints: Vec<(String, String)>,
}

struct SubtitleFile(PathBuf);

impl State {
    fn accepts(&self, session: u64, selection: u64) -> bool {
        self.session == session && self.selection == selection
    }
}
impl Drop for SubtitleFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn choices(
    provider: &str,
    bytes: &[u8],
    preferred: &[String],
) -> Result<Vec<OnlineSubtitle>, String> {
    let mut subtitles = Addon::parse_subtitles(bytes).map_err(|error| error.to_string())?;
    subtitles.sort_by_key(|subtitle| {
        let language = subtitle
            .lang
            .as_deref()
            .and_then(nova_config::playback_preferences::normalize_language);
        preferred
            .iter()
            .position(|wanted| Some(wanted) == language.as_ref())
            .unwrap_or(usize::MAX)
    });
    let mut seen = HashSet::new();
    Ok(subtitles
        .into_iter()
        .filter(|subtitle| subtitle.is_remote() && seen.insert(subtitle.url.clone()))
        .take(200)
        .map(|subtitle| OnlineSubtitle {
            url: subtitle.url,
            language: subtitle
                .lang
                .as_deref()
                .and_then(nova_config::playback_preferences::normalize_language)
                .unwrap_or_else(|| "und".into()),
            title: format!(
                "{} · {}",
                provider,
                subtitle
                    .extra
                    .get("subtitleFileName")
                    .or_else(|| subtitle.extra.get("movieReleaseName"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&subtitle.id)
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(160)
                    .collect::<String>()
            ),
        })
        .collect())
}

fn download(url: &str) -> Result<SubtitleFile, String> {
    let bytes = net::fetch_metadata_bytes(url, Duration::from_secs(20), 8 * 1024 * 1024)
        .map_err(|error| error.to_string())?;
    if bytes.is_empty() {
        return Err("empty subtitle file".into());
    }
    let directory = app_cache_dir().join("subtitles");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = directory.join(format!("{}-{nonce}.sub", std::process::id()));
    use std::io::Write;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| error.to_string())?;
    let file = SubtitleFile(path);
    output
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    Ok(file)
}

impl Bridge {
    pub(super) fn clear_addon_subtitles(&self) {
        self.shared.lock().unwrap().addon_subtitles = State::default();
        self.player.set_online_subtitles(Vec::new());
        if let Some(app) = self.app() {
            app.set_subtitle_addon_status(0);
        }
    }

    pub(super) fn start_addon_subtitles(
        &self,
        media_type: &str,
        id: &str,
        hints: &[(String, String)],
    ) {
        self.clear_addon_subtitles();
        let session = self.player.screenshot_session();
        let hints_ref = hints
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        let preferred = nova_config::playback_preferences::language_preferences(
            &self
                .shared
                .lock()
                .unwrap()
                .cache_settings
                .default_subtitle_language,
        );
        let targets = {
            let mut state = self.shared.lock().unwrap();
            let targets = state
                .installed
                .iter()
                .filter(|addon| {
                    addon.enabled
                        && addon.available
                        && addon.manifest.accepts("subtitles", media_type, id)
                })
                .filter_map(|addon| {
                    Addon::new(&addon.url).ok().map(|client| {
                        (
                            addon.url.clone(),
                            addon.generation,
                            addon.label.clone(),
                            client.subtitles_url(media_type, id, &hints_ref),
                        )
                    })
                })
                .take(16)
                .collect::<VecDeque<_>>();
            state.addon_subtitles.session = session;
            state.addon_subtitles.request = Some(Lookup {
                media_type: media_type.into(),
                id: id.into(),
                hints: hints.to_vec(),
            });
            state.addon_subtitles.pending = targets.len();
            targets
        };
        if targets.is_empty() {
            return;
        }
        if let Some(app) = self.app() {
            app.set_subtitle_addon_status(1);
        }
        // A small worker pool keeps a slow provider from delaying every other
        // addon, while bounding connections independently of installed count.
        let targets = Arc::new(Mutex::new(targets));
        for _ in 0..3 {
            let targets = targets.clone();
            let bridge = self.clone();
            let preferred = preferred.clone();
            thread::spawn(move || {
                loop {
                    let Some((owner, generation, label, url)) = targets.lock().unwrap().pop_front()
                    else {
                        break;
                    };
                    if bridge.player.screenshot_session() != session {
                        break;
                    }
                    let result =
                        net::fetch_metadata_bytes(&url, Duration::from_secs(15), 1024 * 1024)
                            .map_err(|error| error.to_string())
                            .and_then(|bytes| choices(&label, &bytes, &preferred));
                    let bridge = bridge.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(app) = bridge.app().filter(|app| app.get_player_open()) else {
                            return;
                        };
                        if bridge.player.screenshot_session() != session {
                            return;
                        }
                        let (rows, status) = {
                            let mut state = bridge.shared.lock().unwrap();
                            if state.addon_subtitles.session != session {
                                return;
                            }
                            let enabled = state.installed.iter().any(|addon| {
                                addon.url == owner
                                    && addon.generation == generation
                                    && addon.enabled
                                    && addon.available
                            });
                            let state = &mut state.addon_subtitles;
                            state.pending = state.pending.saturating_sub(1);
                            match result {
                                Ok(rows) if enabled => {
                                    for row in rows {
                                        if state.choices.len() < 200
                                            && !state
                                                .choices
                                                .iter()
                                                .any(|existing| existing.url == row.url)
                                        {
                                            state.owners.insert(
                                                row.url.clone(),
                                                (owner.clone(), generation),
                                            );
                                            state.choices.push(row);
                                        }
                                    }
                                }
                                Err(_) => state.failed += 1,
                                _ => {}
                            }
                            let status = if !state.choices.is_empty() {
                                0
                            } else if state.pending > 0 {
                                1
                            } else if state.failed > 0 {
                                4
                            } else {
                                3
                            };
                            (state.choices.clone(), status)
                        };
                        bridge.player.set_online_subtitles(rows);
                        if app.get_subtitle_addon_status() != 2 {
                            app.set_subtitle_addon_status(status);
                        }
                    });
                }
            });
        }
    }

    pub(super) fn cancel_subtitle_download(&self) {
        let mut state = self.shared.lock().unwrap();
        state.addon_subtitles.selection = state.addon_subtitles.selection.wrapping_add(1);
        state.addon_subtitles.automatic_attempted = true;
        drop(state);
        if let Some(app) = self
            .app()
            .filter(|app| app.get_subtitle_addon_status() == 2)
        {
            app.set_subtitle_addon_status(0);
        }
    }

    pub(super) fn retry_addon_subtitles(&self) {
        if !self
            .app()
            .is_some_and(|app| app.get_subtitle_addon_status() == 4)
        {
            return;
        }
        let request = {
            let state = self.shared.lock().unwrap();
            state
                .addon_subtitles
                .choices
                .is_empty()
                .then(|| state.addon_subtitles.request.clone())
                .flatten()
        };
        if let Some(lookup) = request {
            self.start_addon_subtitles(&lookup.media_type, &lookup.id, &lookup.hints);
        }
    }

    pub(super) fn poll_addon_subtitles(&self) {
        if !self
            .app()
            .is_some_and(|app| app.get_player_open() && app.get_playback_started())
        {
            return;
        }
        {
            let mut state = self.shared.lock().unwrap();
            let state = &mut state.addon_subtitles;
            if state.session != self.player.screenshot_session()
                || state.pending > 0
                || state.automatic_attempted
            {
                return;
            }
            state.automatic_attempted = true;
        }
        if let Some(id) = self.player.automatic_online_subtitle() {
            self.load_addon_subtitle(id, false);
        }
    }

    pub(super) fn load_addon_subtitle(&self, id: i32, manual: bool) {
        let Some(subtitle) = self.player.online_subtitle(id) else {
            return;
        };
        let session = self.player.screenshot_session();
        let (selection, owner, generation) = {
            let mut state = self.shared.lock().unwrap();
            let Some((owner, generation)) = state.addon_subtitles.owners.get(&subtitle.url) else {
                return;
            };
            if !state.installed.iter().any(|addon| {
                addon.url == *owner
                    && addon.generation == *generation
                    && addon.enabled
                    && addon.available
            }) {
                return;
            }
            let (owner, generation) = (owner.clone(), *generation);
            let state = &mut state.addon_subtitles;
            state.selection = state.selection.wrapping_add(1);
            (state.selection, owner, generation)
        };
        if let Some(app) = self.app() {
            app.set_subtitle_addon_status(2);
        }
        let bridge = self.clone();
        thread::spawn(move || {
            let result = download(&subtitle.url);
            let _ = slint::invoke_from_event_loop(move || {
                let Some(app) = bridge.app().filter(|app| app.get_player_open()) else {
                    return;
                };
                if bridge.player.screenshot_session() != session {
                    return;
                }
                {
                    let state = bridge.shared.lock().unwrap();
                    if !state.addon_subtitles.accepts(session, selection)
                        || !state.installed.iter().any(|addon| {
                            addon.url == owner
                                && addon.generation == generation
                                && addon.enabled
                                && addon.available
                        })
                    {
                        return;
                    }
                }
                let result = result.and_then(|file| {
                    let id = bridge.player.add_online_subtitle(
                        &file.0.to_string_lossy(),
                        &subtitle.title,
                        &subtitle.language,
                    )?;
                    bridge
                        .shared
                        .lock()
                        .unwrap()
                        .addon_subtitles
                        .files
                        .push(file);
                    Ok(id)
                });
                match result {
                    Ok(id) => {
                        if manual {
                            bridge.pick_playback_track(false, id);
                        } else {
                            bridge.player.pick_sub_track(id);
                        }
                        bridge.player.refresh_tracks();
                        app.set_subtitle_addon_status(0);
                    }
                    Err(error) => {
                        eprintln!("nova: subtitle download failed: {error}");
                        app.set_subtitle_addon_status(4);
                    }
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_downloads_cannot_cross_a_track_change_or_playback_session() {
        let mut state = State {
            session: 7,
            selection: 2,
            ..Default::default()
        };
        assert!(state.accepts(7, 2));
        state.selection += 1; // Off or another track while a download is pending.
        assert!(!state.accepts(7, 2));
        state.session += 1;
        state.selection = 2; // The next episode may reuse a selection counter.
        assert!(!state.accepts(7, 2));
    }

    #[test]
    fn preferred_languages_survive_a_large_provider_result() {
        let mut rows = (0..220).map(|id| serde_json::json!({"id": id.to_string(), "url": format!("https://example.com/{id}.srt"), "lang":"eng"})).collect::<Vec<_>>();
        rows.push(serde_json::json!({"id":"croatian","url":"https://example.com/hr.srt","lang":"hrv","subtitleFileName":"Movie.1080p.srt"}));
        let rows = choices(
            "OpenSubtitles",
            &serde_json::to_vec(&serde_json::json!({"subtitles":rows})).unwrap(),
            &["hr".into()],
        )
        .unwrap();
        assert_eq!(rows.len(), 200);
        assert_eq!(rows[0].language, "hr");
        assert_eq!(rows[0].title, "OpenSubtitles · Movie.1080p.srt");
    }

    #[test]
    fn choices_reject_local_files_deduplicate_and_normalize_languages() {
        let rows = choices(
            "OpenSubtitles",
            br#"{"subtitles":[
            {"id":"one","url":"https://example.com/sub.srt","lang":"eng"},
            {"id":"duplicate","url":"https://example.com/sub.srt","lang":"en"},
            {"id":"local","url":"file:///etc/passwd","lang":"eng"},
            {"id":"two","url":"https://example.com/hr.ass","lang":"hrv"}
        ]}"#,
            &[],
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].language, "en");
        assert_eq!(rows[1].language, "hr");
        assert_eq!(rows[0].title, "OpenSubtitles · one");
    }
}

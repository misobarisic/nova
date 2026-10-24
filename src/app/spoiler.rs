//! Presentation-only spoiler policy. Watched rows come from the resolved
//! profile projection, never numbering, position or unverified provider labels.
use super::*;

type RevealKey = (String, u64, String, String, String);
static REVEALED: LazyLock<Mutex<HashSet<RevealKey>>> = LazyLock::new(Mutex::default);
fn reveal_key(media_type: &str, title: &str, episode: &str) -> RevealKey {
    let context = profiles::capture();
    (
        context.id,
        context.epoch,
        media_type.into(),
        title.into(),
        episode.into(),
    )
}
pub(super) fn reveal(media_type: &str, title: &str, episode: &str) {
    REVEALED
        .lock()
        .unwrap()
        .insert(reveal_key(media_type, title, episode));
}
pub(super) fn revealed(media_type: &str, title: &str, episode: &str) -> bool {
    REVEALED
        .lock()
        .unwrap()
        .contains(&reveal_key(media_type, title, episode))
}
/// Lockscreen presentation deliberately ignores transient foreground reveals.
pub(super) fn notification_hidden(
    state: &Shared,
    media_type: &str,
    title: &str,
    episode: &str,
) -> bool {
    state.cache_settings.spoiler_protection
        && media_type != "movie"
        && !state
            .progress
            .get(&progress_map_key(title, episode))
            .is_some_and(|p| p.watched)
}
/// The Android download service is device-wide. The selected profile cannot
/// authorize another owner's episode metadata; use generic progress without
/// reading other profiles on the UI tick. Ownership may survive a manual keep.
#[cfg(any(target_os = "android", test))]
pub(super) fn download_notification_hidden(
    locally_protected: bool,
    active_profile: &str,
    job: &DownloadJob,
) -> bool {
    job.offline
        .owners
        .iter()
        .any(|owner| owner.profile != active_profile)
        || locally_protected
}

pub(super) fn hidden(state: &Shared, media_type: &str, title: &str, episode: &str) -> bool {
    notification_hidden(state, media_type, title, episode) && !revealed(media_type, title, episode)
}
pub(super) fn matches_filter(
    state: &Shared,
    modal: &ModalItem,
    video: &Video,
    filter: &str,
) -> bool {
    if !hidden(state, &modal.type_, &modal.id, &video.id) {
        return episode_matches_filter(video, filter);
    }
    let needle = filter.trim().to_lowercase();
    needle.is_empty()
        || episode_se_label(video).to_lowercase().contains(&needle)
        || video.episode_number().is_some_and(|number| {
            number.to_string() == needle
                || format!("ep {number}") == needle
                || format!("e{number}") == needle
        })
}

/// Download metadata may be missing; raw filenames/provider strings and opaque
/// request IDs are never a safe presentation fallback for protected episodes.
pub(super) fn numbered_download_label(video: Option<&Video>, request_id: &str) -> String {
    if let Some(video) = video {
        return episode_se_label(video);
    }
    let mut parts = request_id.rsplit(':');
    let numbers = parts
        .next()
        .and_then(|episode| episode.parse::<u32>().ok())
        .zip(parts.next().and_then(|season| season.parse::<u32>().ok()));
    numbers.map_or_else(
        || text::tr("Episode").into(),
        |(episode, season)| format!("S{season} E{episode}"),
    )
}

pub(super) fn artwork_allowed(
    state: &Shared,
    media_type: &str,
    title: &str,
    episode: &str,
) -> bool {
    !hidden(state, media_type, title, episode)
        && (state.cache_settings.show_unwatched_thumbs
            || state
                .progress
                .get(&progress_map_key(title, episode))
                .is_some_and(|p| p.watched || p.position_secs > 0.0))
}
/// Foreground reveals do not authorize lock-screen or calendar export leaks.
pub(super) fn owner_enabled(owner: &profiles::ProfileStore) -> bool {
    // The UI captures preferences before the debounced disk save. A pending
    // enable must already redact foreground reminders and background delivery.
    // The durable preference remains authoritative for inactive profiles.
    let pending_enable =
        owner.id() == profiles::active_context().id && active_cache_settings().spoiler_protection;
    pending_enable
        || match owner.read("settings") {
            Ok(None) => false,
            Ok(Some(raw)) => serde_json::from_str::<CacheSettings>(&raw)
                .map(|settings| settings.spoiler_protection)
                .unwrap_or(true),
            Err(_) => true,
        }
}
impl Bridge {
    pub(super) fn refresh_spoiler_presentation(&self) {
        progress_projection::invalidate();
        let (settings, selected, context) = {
            let state = self.shared.lock().unwrap();
            (
                state.cache_settings.clone(),
                state
                    .modal_item
                    .as_ref()
                    .and_then(|m| m.videos.iter().find(|v| v.id == m.request_id))
                    .cloned(),
                state
                    .playback_intent
                    .as_ref()
                    .map(|intent| intent.context.clone()),
            )
        };
        set_active_cache_settings(settings.clone());
        self.refresh_episode_rows();
        self.rebuild_watch_history_list();
        self.apply_home_to_ui();
        self.refresh_party_language_text();
        self.refresh_release_spoilers();
        self.refresh_downloads_page();
        self.downloads_list_to_ui();
        self.refresh_download_background_status();
        if let Some(app) = self.app() {
            if let Some(video) = selected {
                self.apply_stream_episode_context(&app, Some(&video));
            }
            if settings.spoiler_protection {
                app.set_player_poster(Image::default());
            }
            if let Some(context) = context {
                self.publish_player_context(&context);
            }
        }
    }

    pub(super) fn spoiler_hidden(&self, video: &Video) -> bool {
        let state = self.shared.lock().unwrap();
        state
            .modal_item
            .as_ref()
            .map_or(state.cache_settings.spoiler_protection, |modal| {
                hidden(&state, &modal.type_, &modal.id, &video.id)
            })
    }
    pub(super) fn apply_stream_episode_context(&self, app: &AppWindow, video: Option<&Video>) {
        if let Some(video) = video.filter(|video| self.spoiler_hidden(video)) {
            app.set_episode_context(episode_se_label(video).into());
            app.set_stream_episode_number(episode_se_label(video).into());
            app.set_stream_episode_title("".into());
        } else {
            apply_stream_episode_context(app, video);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protection_uses_exact_watched_or_explicit_scoped_reveal() {
        let mut state = Shared::default();
        assert!(!hidden(&state, "series", "spoiler-policy", "episode-a"));
        state.cache_settings.spoiler_protection = true;
        assert!(hidden(&state, "series", "spoiler-policy", "episode-a"));
        let progress = EpisodeProgress {
            series_id: "spoiler-policy".into(),
            episode_id: "episode-a".into(),
            position_secs: 999.0,
            duration_secs: 1000.0,
            ..Default::default()
        };
        state
            .progress
            .insert(progress_map_key("spoiler-policy", "episode-a"), progress);
        assert!(hidden(&state, "series", "spoiler-policy", "episode-a"));
        state
            .progress
            .get_mut(&progress_map_key("spoiler-policy", "episode-a"))
            .unwrap()
            .watched = true;
        assert!(!hidden(&state, "series", "spoiler-policy", "episode-a"));
        state
            .progress
            .get_mut(&progress_map_key("spoiler-policy", "episode-a"))
            .unwrap()
            .watched = false;
        reveal("series", "spoiler-policy", "episode-a");
        assert!(!hidden(&state, "series", "spoiler-policy", "episode-a"));
        assert!(hidden(&state, "series", "spoiler-policy", "episode-b"));
        assert!(hidden(&state, "anime", "spoiler-policy", "episode-a"));
        assert!(hidden(&state, "series", "other-parent", "episode-a"));
        assert!(!state.progress[&progress_map_key("spoiler-policy", "episode-a")].watched);
        let original = profiles::capture();
        profiles::with_context(profiles::context_for("spoiler-other-profile"), || {
            assert!(hidden(&state, "series", "spoiler-policy", "episode-a"))
        });
        profiles::with_context(profiles::activation_context(&original.id), || {
            assert!(hidden(&state, "series", "spoiler-policy", "episode-a"))
        });
    }
}

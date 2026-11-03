//! Presentation-only spoiler policy. Watched rows come from the resolved
//! profile projection, never numbering, position or unverified provider labels.
use super::*;

type RevealKey = (String, u64, String, String, String);
static REVEAL_REVISION: AtomicU64 = AtomicU64::new(1);
#[derive(Default)]
struct RetiredEpochs {
    // Exact closed intervals, sorted and disjoint. Adjacent retired epochs
    // coalesce; unpublished or still-live gaps must never be filled in.
    ranges: Vec<(u64, u64)>,
}
impl RetiredEpochs {
    fn contains(&self, epoch: u64) -> bool {
        let next = self.ranges.partition_point(|&(start, _)| start <= epoch);
        next > 0 && self.ranges[next - 1].1 >= epoch
    }
    fn retire(&mut self, epoch: u64) {
        let next = self.ranges.partition_point(|&(start, _)| start <= epoch);
        if next > 0 && self.ranges[next - 1].1 >= epoch {
            return;
        }
        let left = next > 0 && self.ranges[next - 1].1.checked_add(1) == Some(epoch);
        let right = next < self.ranges.len() && epoch.checked_add(1) == Some(self.ranges[next].0);
        match (left, right) {
            (true, true) => {
                let end = self.ranges.remove(next).1;
                self.ranges[next - 1].1 = end;
            }
            (true, false) => self.ranges[next - 1].1 = epoch,
            (false, true) => self.ranges[next].0 = epoch,
            (false, false) => self.ranges.insert(next, (epoch, epoch)),
        }
    }
}
#[derive(Default)]
struct RevealState {
    entries: HashSet<RevealKey>,
    // Activation epochs are process-unique (epoch 1 is the initial default).
    // Session-free contexts have no cancellation token. Exact numeric ranges
    // reject late work without retaining old strings; arbitrary unpublished
    // epoch gaps can retain multiple ranges rather than one high watermark.
    retired: RetiredEpochs,
}
impl RevealState {
    fn usable(&self, context: &profiles::Context) -> bool {
        !self.retired.contains(context.epoch)
            && !context
                .session
                .as_ref()
                .is_some_and(|session| session.cancelled.load(Ordering::Acquire))
    }
    fn insert(
        &mut self,
        context: &profiles::Context,
        media_type: &str,
        title: &str,
        episode: &str,
    ) -> bool {
        self.usable(context)
            && self.entries.insert((
                context.id.clone(),
                context.epoch,
                media_type.into(),
                title.into(),
                episode.into(),
            ))
    }
    fn retire(&mut self, profile: &str, epoch: u64, cancelled: &AtomicBool) -> bool {
        // This lock orders the cancellation check/insertion and retirement.
        // Never acquire a session/directory/profile lock while holding it;
        // callers capture Context or release the directory write guard first.
        cancelled.store(true, Ordering::Release);
        self.retired.retire(epoch);
        let before = self.entries.len();
        self.entries
            .retain(|(owner, activation, ..)| owner != profile || *activation != epoch);
        before != self.entries.len()
    }
}
static REVEALED: LazyLock<Mutex<RevealState>> = LazyLock::new(Mutex::default);
pub(super) fn reveal(media_type: &str, title: &str, episode: &str) {
    let context = profiles::capture();
    let mut state = REVEALED.lock().unwrap();
    if state.insert(&context, media_type, title, episode) {
        REVEAL_REVISION.fetch_add(1, Ordering::AcqRel);
    }
}
pub(super) fn retire_context(profile: &str, epoch: u64, cancelled: &AtomicBool) {
    let mut state = REVEALED.lock().unwrap();
    if state.retire(profile, epoch, cancelled) {
        REVEAL_REVISION.fetch_add(1, Ordering::AcqRel);
    }
}

pub(super) fn reveal_revision() -> u64 {
    REVEAL_REVISION.load(Ordering::Acquire)
}

/// A scan receives only reveals owned by its exact profile activation.
/// Callers validate reveal_revision again before publishing the snapshot.
pub(super) fn revealed_snapshot(context: &profiles::Context) -> HashSet<(String, String, String)> {
    let state = REVEALED.lock().unwrap();
    if !state.usable(context) {
        return HashSet::new();
    }
    state
        .entries
        .iter()
        .filter(|(profile, epoch, ..)| *profile == context.id && *epoch == context.epoch)
        .map(|(_, _, kind, title, episode)| (kind.clone(), title.clone(), episode.clone()))
        .collect()
}

pub(super) fn revealed(media_type: &str, title: &str, episode: &str) -> bool {
    let context = profiles::capture();
    let state = REVEALED.lock().unwrap();
    state.usable(&context)
        && state.entries.contains(&(
            context.id,
            context.epoch,
            media_type.into(),
            title.into(),
            episode.into(),
        ))
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
        self.refresh_unified_search_presentation();
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
    fn search_reveal_snapshot_keeps_profile_and_activation_ownership() {
        let context = profiles::activation_context("search-reveal-snapshot");
        let before = reveal_revision();
        profiles::with_context(context.clone(), || reveal("series", "title", "episode"));
        assert!(reveal_revision() > before);
        assert!(revealed_snapshot(&context).contains(&(
            "series".into(),
            "title".into(),
            "episode".into()
        )));
        let mut other = context.clone();
        other.epoch += 1;
        assert!(revealed_snapshot(&other).is_empty());
        other.id = "different-search-profile".into();
        other.epoch = context.epoch;
        assert!(revealed_snapshot(&other).is_empty());
    }
    #[test]
    fn protection_uses_exact_watched_or_explicit_scoped_reveal() {
        profiles::with_context(
            profiles::activation_context("spoiler-policy-fixture"),
            || {
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
            },
        );
    }
}

#[cfg(test)]
mod lifetime_tests {
    use super::*;
    use std::sync::{Barrier, mpsc};

    fn context(id: &str, epoch: u64) -> profiles::Context {
        profiles::Context {
            id: id.into(),
            epoch,
            session: None,
        }
    }
    fn retained_string_bytes(state: &RevealState) -> usize {
        state
            .entries
            .iter()
            .map(|(profile, _, kind, title, episode)| {
                profile.capacity() + kind.capacity() + title.capacity() + episode.capacity()
            })
            .sum()
    }
    #[test]
    fn state10_thousand_a_b_a_cycles_keep_only_active_reveal_strings() {
        let mut state = RevealState::default();
        for cycle in 0..1000 {
            for (step, profile) in ["A", "B", "A"].into_iter().enumerate() {
                let epoch = 1 + cycle * 3 + step as u64;
                let active = context(profile, epoch);
                for episode in 0..100 {
                    assert!(state.insert(
                        &active,
                        "series",
                        "shared-title",
                        &format!("episode-{episode}")
                    ));
                }
                assert_eq!(state.entries.len(), 100);
                assert!(
                    state
                        .entries
                        .iter()
                        .all(|key| key.0 == profile && key.1 == epoch)
                );
                if cycle != 999 || step != 2 {
                    let cancelled = AtomicBool::new(false);
                    assert!(state.retire(profile, epoch, &cancelled));
                    assert!(cancelled.load(Ordering::Acquire));
                    assert_eq!(state.entries.len(), 0);
                    assert_eq!(retained_string_bytes(&state), 0);
                    assert!(!state.insert(&active, "series", "stale-title", "stale-episode"));
                }
            }
        }
        assert_eq!(state.entries.len(), 100);
        assert_eq!(state.retired.ranges, [(1, 2999)]);
        assert!(retained_string_bytes(&state) < 10_000);
    }
    #[test]
    fn state10_retiring_old_a_cannot_clear_b_or_new_a_and_identity_stays_typed() {
        let mut state = RevealState::default();
        let old = context("A", 1);
        let b = context("B", 2);
        let new = context("A", 3);
        assert!(state.insert(&old, "series", "title", "episode"));
        assert!(state.insert(&b, "series", "title", "episode"));
        let cancelled = AtomicBool::new(false);
        assert!(state.retire("A", 1, &cancelled));
        assert!(state.insert(&new, "series", "title", "episode"));
        assert!(state.insert(&new, "anime", "title", "episode"));
        assert!(state.insert(&new, "series", "other-title", "episode"));
        assert!(!state.retire("A", 1, &cancelled));
        assert!(!state.insert(&old, "series", "stale", "episode"));
        assert_eq!(state.entries.len(), 4);
        assert!(state.entries.iter().any(|key| key.0 == "B"));
        assert_eq!(state.entries.iter().filter(|key| key.0 == "A").count(), 3);
    }
    #[test]
    fn state10_retirement_intervals_preserve_exact_live_and_unpublished_holes() {
        let mut retired = RetiredEpochs::default();
        for epoch in [1, 4, 6] {
            retired.retire(epoch);
        }
        assert_eq!(retired.ranges, [(1, 1), (4, 4), (6, 6)]);
        for epoch in [0, 2, 3, 5, 7] {
            assert!(!retired.contains(epoch));
        }
        retired.retire(5);
        assert_eq!(retired.ranges, [(1, 1), (4, 6)]);
        assert!(!retired.contains(2));
        assert!(!retired.contains(3));
        retired.retire(3);
        assert_eq!(retired.ranges, [(1, 1), (3, 6)]);
        assert!(!retired.contains(2));
        retired.retire(2);
        retired.retire(4);
        assert_eq!(retired.ranges, [(1, 6)]);
        retired.retire(u64::MAX);
        retired.retire(u64::MAX - 1);
        retired.retire(0);
        assert_eq!(retired.ranges, [(0, 6), (u64::MAX - 1, u64::MAX)]);
        assert!(!retired.contains(7));
        assert!(!retired.contains(u64::MAX - 2));
        // Irregular publication can leave many exact ranges. Missing epochs
        // remain usable even when surrounded by retired activations.
        let mut holes = RetiredEpochs::default();
        for epoch in (0..2000).step_by(2) {
            holes.retire(epoch);
        }
        assert_eq!(holes.ranges.len(), 1000);
        for epoch in 0..2000 {
            assert_eq!(holes.contains(epoch), epoch % 2 == 0);
        }
    }
    #[test]
    fn state10_insertion_and_retirement_have_one_lock_order_in_both_interleavings() {
        let owner = context("A", 1);
        let state = Arc::new(Mutex::new(RevealState::default()));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = mpsc::channel();
        let release = Arc::new(Barrier::new(2));
        let worker = {
            let state = state.clone();
            let release = release.clone();
            std::thread::spawn(move || {
                let owner = context("A", 1);
                let mut guard = state.lock().unwrap();
                entered_tx.send(()).unwrap();
                release.wait();
                assert!(guard.insert(&owner, "series", "title", "episode"));
            })
        };
        entered_rx.recv().unwrap();
        let retire = {
            let state = state.clone();
            let cancelled = cancelled.clone();
            std::thread::spawn(move || {
                state.lock().unwrap().retire("A", 1, &cancelled);
            })
        };
        release.wait();
        worker.join().unwrap();
        retire.join().unwrap();
        assert!(state.lock().unwrap().entries.is_empty());
        assert!(
            !state
                .lock()
                .unwrap()
                .insert(&owner, "series", "late", "episode")
        );
        for epoch in 2..66 {
            let barrier = Arc::new(Barrier::new(3));
            let insert = {
                let state = state.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    state.lock().unwrap().insert(
                        &context("A", epoch),
                        "series",
                        "title",
                        "episode",
                    );
                })
            };
            let retire = {
                let state = state.clone();
                let barrier = barrier.clone();
                let cancelled = cancelled.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    state.lock().unwrap().retire("A", epoch, &cancelled);
                })
            };
            barrier.wait();
            insert.join().unwrap();
            retire.join().unwrap();
            assert!(state.lock().unwrap().entries.is_empty());
        }
    }
    #[test]
    fn state10_retirement_invalidates_reveal_snapshot_revision_and_rejects_late_work() {
        let context = profiles::activation_context("spoiler-retirement-revision");
        profiles::with_context(context.clone(), || reveal("series", "title", "episode"));
        assert_eq!(revealed_snapshot(&context).len(), 1);
        let revision = reveal_revision();
        let cancelled = AtomicBool::new(false);
        retire_context(&context.id, context.epoch, &cancelled);
        assert!(reveal_revision() > revision);
        assert!(revealed_snapshot(&context).is_empty());
        let revision = reveal_revision();
        profiles::with_context(context.clone(), || reveal("series", "late", "episode"));
        assert_eq!(reveal_revision(), revision);
        assert!(revealed_snapshot(&context).is_empty());
    }
    #[test]
    fn state10_foreground_reveal_never_authorizes_background_metadata() {
        profiles::with_context(
            profiles::activation_context("spoiler-background-fixture"),
            || {
                let mut state = Shared::default();
                state.cache_settings.spoiler_protection = true;
                reveal("series", "title", "episode");
                assert!(!hidden(&state, "series", "title", "episode"));
                assert!(notification_hidden(&state, "series", "title", "episode"));
                assert!(state.progress.is_empty());
            },
        );
    }
}

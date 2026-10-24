//! Append-shaped, profile-owned playback sessions and honest viewing summaries.
//!
//! Existing progress is intentionally never imported: it cannot tell us when or
//! how long somebody watched. Each session has its own durable key, so later
//! recaps can grow without replacing the whole history on every checkpoint.
use super::*;
use nova_ui_shared::{JournalBridge, JournalMonthRow, JournalSessionRow, JournalTitleRow};
use std::collections::BTreeMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(super) const PREFIX: &str = "viewing_journal:v1:";
const CHECKPOINT_SECONDS: f64 = 15.0;
const LOAD_RETRY_DELAY: Duration = Duration::from_secs(5);
static NEXT_ID: AtomicU64 = AtomicU64::new(0);
// Final checkpoints serialize only with projections of the same owner. A
// background archive for another profile must not hold up local Stop.
type WriterRefs = HashMap<String, std::sync::Weak<Mutex<()>>>;
static WRITERS: LazyLock<Mutex<WriterRefs>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn writer(owner: &profiles::ProfileStore) -> Arc<Mutex<()>> {
    let mut writers = WRITERS.lock().unwrap();
    writers.retain(|_, writer| writer.strong_count() > 0);
    let key = owner.domain(activity_sync::STATS);
    if let Some(writer) = writers.get(&key).and_then(std::sync::Weak::upgrade) {
        return writer;
    }
    let writer = Arc::new(Mutex::new(()));
    writers.insert(key, Arc::downgrade(&writer));
    writer
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct ViewingSession {
    id: String,
    media_type: String,
    title_id: String,
    episode_id: String,
    title: String,
    episode: String,
    started_at: u64,
    updated_at: u64,
    ended_at: Option<u64>,
    watched_seconds: f64,
    /// UTC month allocations keep a session spanning New Year in both recaps.
    months: BTreeMap<String, f64>,
    days: BTreeMap<u64, f64>,
    completed_at: Option<u64>,
    #[serde(default)]
    title_completed_at: Option<u64>,
    rewatch: bool,
    #[serde(default)]
    replay: Option<previously_watched::ReplayEvidence>,
    #[serde(default)]
    viewing_run_start: bool,
}

impl ViewingSession {
    fn matches(&self, media: &nova_playback_api::MediaIdentity) -> bool {
        self.media_type == media.media_type
            && self.title_id == media.title_id
            && self.episode_id == media.episode_id
    }

    fn seconds_in(&self, year: i32) -> f64 {
        if year == 0 {
            self.watched_seconds
        } else {
            let prefix = format!("{year:04}-");
            self.months
                .iter()
                .filter(|(month, _)| month.starts_with(&prefix))
                .map(|(_, seconds)| seconds)
                .sum()
        }
    }
}

struct ActiveSession {
    row: ViewingSession,
    owner: profiles::ProfileStore,
    last_tick: Instant,
    last_position: f64,
    was_playing: bool,
    completion_eligible: bool,
    history_known: bool,
    started_position: f64,
    media_duration: f64,
    saved_seconds: f64,
}

#[derive(Default)]
pub(super) struct JournalTracker {
    active: Option<ActiveSession>,
    // Previously persisted records are loaded once per activation, not per tick.
    loaded: bool,
    retry_at: Option<Instant>,
    rows: BTreeMap<String, ViewingSession>,
    error: bool,
    sync_generation: u64,
}

fn unique_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{:x}-{nanos:x}-{:x}-{:x}",
        nova_sync::local_device().unwrap_or(u64::from(std::process::id())),
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn year_at(at: u64) -> i32 {
    super::home::civil_from_days((at / 86400) as i64).0 as i32
}

fn month_at(at: u64) -> String {
    let (year, month, _) = super::home::civil_from_days((at / 86400) as i64);
    format!("{year:04}-{month:02}")
}

/// Count time spent watching, not distance travelled through the media. Long
/// background gaps and discontinuities (including seeks) contribute nothing.
fn watched_interval(elapsed: f64, previous: f64, position: f64, playing: bool, speed: f64) -> f64 {
    let delta = position - previous;
    if playing
        && elapsed.is_finite()
        && (0.0..=5.0).contains(&elapsed)
        && previous.is_finite()
        && position.is_finite()
        && speed.is_finite()
        && speed > 0.0
        && delta > 0.0
        && delta <= elapsed * speed * 1.5 + 0.35
    {
        // Very short position changes near pause/EOF cannot credit a full tick.
        elapsed.min(delta / speed)
    } else {
        0.0
    }
}

fn load(owner: &profiles::ProfileStore) -> Result<(BTreeMap<String, ViewingSession>, bool), ()> {
    let records = owner.scan(PREFIX).map_err(|error| {
        storage::report(error);
    })?;
    let mut result = BTreeMap::new();
    let mut unreadable = false;
    for (_, raw) in records {
        match serde_json::from_str::<ViewingSession>(&raw) {
            Ok(row) if row.watched_seconds.is_finite() && row.watched_seconds >= 0.0 => {
                result.insert(row.id.clone(), row);
            }
            _ => unreadable = true, // Preserve bad records while retaining readable sessions.
        }
    }
    Ok((result, unreadable))
}

/// Read only actual playback sessions; progress flags are never history evidence.
pub(super) fn last_time(
    owner: &profiles::ProfileStore,
    type_: &str,
    id: &str,
) -> previously_watched::Facts {
    let Ok((rows, unreadable)) = load(owner) else {
        return previously_watched::Facts::default();
    };
    let matching: Vec<_> = rows
        .values()
        .filter(|r| r.media_type == type_ && r.title_id == id && r.watched_seconds > 0.0)
        .collect();
    let now = now_secs();
    let last_at = matching
        .iter()
        .filter_map(|r| last_playback_date(r, now))
        .max();
    // A restart of an already completed episode establishes a new viewing run.
    // Never pull a completion from before that restart into its recap.
    project_last_time(matching, unreadable, last_at)
}

/// Old versions stamped Stop into updated_at, including after an overnight
/// pause. Positive UTC allocations remain actual playback evidence. The sheet
/// shows a date only, so return the corresponding midnight, never the Stop time.
fn last_playback_date(row: &ViewingSession, now: u64) -> Option<u64> {
    let first_day = row.started_at / 86400;
    let last_day = (row.updated_at / 86400).min(now / 86400);
    row.days
        .iter()
        .filter(|(day, seconds)| {
            **day >= first_day && **day <= last_day && seconds.is_finite() && **seconds > 0.0
        })
        .map(|(day, _)| *day)
        .max()
        .or_else(|| {
            row.completed_at
                .filter(|at| *at >= row.started_at && *at <= now)
                .map(|at| at / 86400)
        })
        .and_then(|day| day.checked_mul(86400))
}

fn project_last_time(
    matching: Vec<&ViewingSession>,
    unreadable: bool,
    last_at: Option<u64>,
) -> previously_watched::Facts {
    let run_start = matching
        .iter()
        .filter(|r| r.viewing_run_start)
        .map(|r| r.started_at)
        .max();
    let unknown_rewatch = matching.iter().any(|r| r.rewatch) && run_start.is_none();
    let latest = matching
        .iter()
        .filter(|r| {
            r.completed_at
                .is_some_and(|at| run_start.is_none_or(|start| at >= start))
        })
        .max_by_key(|r| (r.completed_at, r.updated_at, &r.id));
    previously_watched::Facts {
        last_at,
        completed: (!unreadable && !unknown_rewatch)
            .then_some(latest)
            .flatten()
            .map(|r| previously_watched::Completed {
                episode_id: r.episode_id.clone(),
                replay: r.replay.clone(),
            }),
    }
}

pub(super) fn validate_session(key: &str, raw: &str) -> nova_sync::Result<String> {
    let row: ViewingSession = serde_json::from_str(raw)?;
    if key.strip_prefix(PREFIX) != Some(row.id.as_str())
        || row.id.is_empty()
        || row.id.len() > 256
        || !row.watched_seconds.is_finite()
        || row.watched_seconds < 0.0
        || row.replay.as_ref().is_some_and(|e| !e.stored_valid())
        || row
            .months
            .values()
            .chain(row.days.values())
            .any(|seconds| !seconds.is_finite() || *seconds < 0.0)
    {
        return Err(storage::Error::new(
            storage::ErrorKind::Schema,
            "invalid synced viewing session",
        )
        .into());
    }
    Ok(row.id)
}

pub(super) fn session_can_replace(old: &str, new: &str) -> nova_sync::Result<bool> {
    let old: ViewingSession = serde_json::from_str(old)?;
    let new: ViewingSession = serde_json::from_str(new)?;
    Ok(session_can_replace_rows(&old, &new))
}

fn session_can_replace_rows(old: &ViewingSession, new: &ViewingSession) -> bool {
    !((old.viewing_run_start && !new.viewing_run_start)
        || old.watched_seconds > new.watched_seconds
        || (old.title_completed_at.is_some() && new.title_completed_at.is_none())
        || (old.watched_seconds == new.watched_seconds
            && old.ended_at.is_some()
            && new.ended_at.is_none()))
}

fn persist_now(owner: &profiles::ProfileStore, row: &ViewingSession) {
    // Independent checkpoints may arrive on different workers. Never let a
    // delayed one overwrite a newer duration or a completed/finalized row.
    let writer = writer(owner);
    let _serial = writer.lock().unwrap();
    let key = format!("{PREFIX}{}", row.id);
    let prior = match owner.read(&key) {
        Ok(Some(raw)) => match serde_json::from_str::<ViewingSession>(&raw) {
            Ok(row) => Some(row),
            Err(error) => {
                tracing::warn!(%error, "unreadable viewing session preserved");
                return;
            }
        },
        Ok(None) => None,
        Err(error) => {
            storage::report(error);
            return;
        }
    };
    if prior.as_ref().is_some_and(|old| {
        (old.viewing_run_start && !row.viewing_run_start)
            || (old.title_completed_at.is_some() && row.title_completed_at.is_none())
            || old.watched_seconds > row.watched_seconds
            || (old.watched_seconds == row.watched_seconds
                && old.ended_at.is_some()
                && row.ended_at.is_none())
    }) {
        return;
    }
    match serde_json::to_string(row) {
        Ok(raw) => {
            if let Err(error) = activity_sync::persist_session(owner, &key, &raw) {
                storage::report(storage::Error::new(storage::ErrorKind::Transaction, error));
            }
        }
        Err(error) => tracing::warn!(%error, "viewing journal serialization failed"),
    }
}

fn persist(owner: profiles::ProfileStore, row: ViewingSession) {
    profiles::thread::spawn(move || persist_now(&owner, &row));
}

/// A completed title only becomes a rewatch when playback starts near its
/// beginning, or resumes an unfinished rewatch recorded after that completion.
fn rewatch_state<'a>(
    rows: impl Iterator<Item = &'a ViewingSession>,
    media: &nova_playback_api::MediaIdentity,
    position: f64,
    media_duration: f64,
) -> (bool, bool) {
    let matching = rows.filter(|row| row.matches(media)).collect::<Vec<_>>();
    let completed_at = matching.iter().filter_map(|row| row.completed_at).max();
    let Some(completed_at) = completed_at else {
        return (false, true);
    };
    let resumed_rewatch = matching
        .iter()
        .any(|row| row.rewatch && row.completed_at.is_none() && row.started_at >= completed_at);
    let rewatch = resumed_rewatch || (media_duration > 0.0 && position <= media_duration * 0.2);
    (rewatch, rewatch)
}

/// A rewatched successor belongs to the same run; another completion of an
/// episode already replayed in that run establishes a later restart.
fn starts_viewing_run<'a>(
    rows: impl Iterator<Item = &'a ViewingSession>,
    media: &nova_playback_api::MediaIdentity,
) -> bool {
    let matching: Vec<_> = rows
        .filter(|r| r.media_type == media.media_type && r.title_id == media.title_id)
        .collect();
    let last_start = matching
        .iter()
        .filter(|r| r.viewing_run_start)
        .map(|r| r.started_at)
        .max();
    match last_start {
        None => true,
        Some(start) => matching.iter().any(|r| {
            r.episode_id == media.episode_id
                && r.rewatch
                && r.completed_at.is_some_and(|at| at >= start)
        }),
    }
}

impl JournalTracker {
    fn ensure_loaded(&mut self, owner: &profiles::ProfileStore) {
        let recovering_active = self
            .active
            .as_ref()
            .is_some_and(|active| !active.history_known);
        self.try_load(Instant::now(), || load(owner));
        if recovering_active
            && let Some(active) = self
                .active
                .as_ref()
                .filter(|active| active.history_known && active.row.watched_seconds > 0.0)
        {
            // Persist the corrected classification even when recovery happened
            // while paused, before another watched-time checkpoint is due.
            persist(active.owner.clone(), active.row.clone());
        }
    }

    fn try_load(
        &mut self,
        now: Instant,
        read: impl FnOnce() -> Result<(BTreeMap<String, ViewingSession>, bool), ()>,
    ) {
        if self.loaded || self.retry_at.is_some_and(|retry| now < retry) {
            return;
        }
        match read() {
            Ok((rows, unreadable)) => {
                // Playback may already have created/checkpointed new sessions
                // while a transient storage read was failing. Their in-memory
                // facts are newer than any persisted copy of the same ID.
                for (id, row) in rows {
                    if self
                        .rows
                        .get(&id)
                        .is_none_or(|old| session_can_replace_rows(old, &row))
                    {
                        self.rows.insert(id, row);
                    }
                }
                self.loaded = true;
                self.retry_at = None;
                self.error = unreadable;
                self.reconcile_active_history();
            }
            Err(()) => {
                self.error = true;
                self.retry_at = Some(now + LOAD_RETRY_DELAY);
            }
        }
    }

    fn reconcile_active_history(&mut self) {
        let Some(active) = self
            .active
            .as_ref()
            .filter(|active| self.loaded && !active.history_known)
        else {
            return;
        };
        let media = nova_playback_api::MediaIdentity {
            media_type: active.row.media_type.clone(),
            title_id: active.row.title_id.clone(),
            episode_id: active.row.episode_id.clone(),
            title: active.row.title.clone(),
            episode: active.row.episode.clone(),
        };
        let (rewatch, eligible) = rewatch_state(
            self.rows.values().filter(|row| row.id != active.row.id),
            &media,
            active.started_position,
            active.media_duration,
        );
        let viewing_run_start = rewatch
            && starts_viewing_run(
                self.rows.values().filter(|row| row.id != active.row.id),
                &media,
            );
        let active = self.active.as_mut().unwrap();
        active.row.rewatch = rewatch;
        active.row.viewing_run_start |= viewing_run_start;
        active.completion_eligible = eligible;
        active.history_known = true;
        if active.row.watched_seconds > 0.0 {
            self.rows.insert(active.row.id.clone(), active.row.clone());
        }
    }

    fn finish(&mut self, at: u64) {
        if let Some(mut active) = self.active.take()
            && active.row.watched_seconds > 0.0
        {
            active.row.ended_at = Some(at);
            // Finalization is not another playback observation. Preserve the
            // actual last-watched date even after a long pause.
            self.rows.insert(active.row.id.clone(), active.row.clone());
            // Finalization is durable before Stop, profile switch or process
            // shutdown returns; background checkpoints cannot overwrite it.
            persist_now(&active.owner, &active.row);
        }
    }

    fn observe(
        &mut self,
        media: nova_playback_api::MediaIdentity,
        owner: profiles::ProfileStore,
        position: f64,
        media_duration: f64,
        playing: bool,
        speed: f64,
    ) -> bool {
        let at = now_secs();
        self.ensure_loaded(&owner);
        if self
            .active
            .as_ref()
            .is_some_and(|active| !active.row.matches(&media) || active.owner != owner)
        {
            self.finish(at);
        }
        let tick = Instant::now();
        if self.active.is_none() {
            if !playing {
                return false;
            }
            let (rewatch, completion_eligible) =
                rewatch_state(self.rows.values(), &media, position, media_duration);
            let viewing_run_start =
                rewatch && self.loaded && starts_viewing_run(self.rows.values(), &media);
            self.active = Some(ActiveSession {
                row: ViewingSession {
                    id: unique_id(),
                    media_type: media.media_type,
                    title_id: media.title_id,
                    episode_id: media.episode_id,
                    title: media.title,
                    episode: media.episode,
                    started_at: at,
                    updated_at: at,
                    ended_at: None,
                    watched_seconds: 0.0,
                    months: BTreeMap::new(),
                    days: BTreeMap::new(),
                    completed_at: None,
                    title_completed_at: None,
                    rewatch,
                    replay: None,
                    viewing_run_start,
                },
                owner,
                last_tick: tick,
                last_position: position,
                was_playing: playing,
                completion_eligible: self.loaded && completion_eligible,
                history_known: self.loaded,
                started_position: position,
                media_duration,
                saved_seconds: -CHECKPOINT_SECONDS,
            });
            return false;
        }
        if media_duration.is_finite() && media_duration > 0.0 {
            self.active.as_mut().unwrap().media_duration = media_duration;
        }
        self.reconcile_active_history();
        let restart_boundary = self
            .active
            .as_ref()
            .filter(|active| {
                active.history_known
                    && !active.completion_eligible
                    && media_duration > 0.0
                    && (position <= media_duration * 0.2
                        || active.started_position <= media_duration * 0.2)
            })
            .is_some_and(|active| {
                starts_viewing_run(
                    self.rows.values().filter(|row| row.id != active.row.id),
                    &nova_playback_api::MediaIdentity {
                        media_type: active.row.media_type.clone(),
                        title_id: active.row.title_id.clone(),
                        episode_id: active.row.episode_id.clone(),
                        ..Default::default()
                    },
                )
            });
        let active = self.active.as_mut().unwrap();
        let watched = watched_interval(
            tick.duration_since(active.last_tick).as_secs_f64(),
            active.last_position,
            position,
            playing && active.was_playing,
            speed,
        );
        active.last_tick = tick;
        active.last_position = position;
        active.was_playing = playing;
        if active.history_known
            && !active.completion_eligible
            && media_duration > 0.0
            && (position <= media_duration * 0.2 || active.started_position <= media_duration * 0.2)
        {
            active.row.rewatch = true;
            active.row.viewing_run_start |= restart_boundary;
            active.completion_eligible = true;
        }
        if watched <= 0.0 {
            return false;
        }
        active.row.watched_seconds += watched;
        active.row.updated_at = at;
        *active.row.months.entry(month_at(at)).or_default() += watched;
        *active.row.days.entry(at / 86400).or_default() += watched;
        let completed = active.completion_eligible
            && media_duration.is_finite()
            && media_duration > 0.0
            && position >= media_duration * 0.9
            && active.row.watched_seconds >= (media_duration * 0.1).min(30.0);
        let new_completion = completed && active.row.completed_at.is_none();
        if new_completion {
            active.row.completed_at = Some(at);
        }
        self.rows.insert(active.row.id.clone(), active.row.clone());
        if new_completion || active.row.watched_seconds - active.saved_seconds >= CHECKPOINT_SECONDS
        {
            active.saved_seconds = active.row.watched_seconds;
            persist(active.owner.clone(), active.row.clone());
        }
        new_completion
    }
}

impl Bridge {
    pub(super) fn reload_synced_viewing_journal(&self) {
        let bridge = self.clone();
        let owner = profiles::ProfileStore::capture();
        let generation = {
            let mut state = self.shared.lock().unwrap();
            state.viewing_journal.sync_generation =
                state.viewing_journal.sync_generation.wrapping_add(1);
            state.viewing_journal.sync_generation
        };
        // Session archives can be large. Decode them outside the UI lock and
        // preserve newer active/checkpoint state if playback advances meanwhile.
        profiles::thread::spawn(move || {
            let deleted = match activity_sync::project_sessions(&owner) {
                Ok(deleted) => deleted,
                Err(error) => {
                    storage::report(storage::Error::new(storage::ErrorKind::Transaction, error));
                    return;
                }
            };
            let loaded = load(&owner);
            let _ = profiles::invoke_from_event_loop(move || {
                if bridge
                    .shared
                    .lock()
                    .unwrap()
                    .viewing_journal
                    .sync_generation
                    != generation
                {
                    return;
                }
                if let Ok((rows, unreadable)) = loaded {
                    let mut state = bridge.shared.lock().unwrap();
                    let tracker = &mut state.viewing_journal;
                    for id in &deleted {
                        tracker.rows.remove(id);
                    }
                    if tracker
                        .active
                        .as_ref()
                        .is_some_and(|active| deleted.contains(&active.row.id))
                    {
                        tracker.active = None;
                    }
                    for (id, row) in rows {
                        if tracker
                            .rows
                            .get(&id)
                            .is_none_or(|old| session_can_replace_rows(old, &row))
                        {
                            tracker.rows.insert(id, row);
                        }
                    }
                    if let Some(active) = &tracker.active
                        && active.row.watched_seconds > 0.0
                    {
                        tracker
                            .rows
                            .insert(active.row.id.clone(), active.row.clone());
                    }
                    tracker.loaded = true;
                    tracker.error = unreadable;
                    tracker.retry_at = None;
                    tracker.reconcile_active_history();
                    drop(state);
                    if bridge
                        .app()
                        .is_some_and(|app| app.global::<JournalBridge>().get_open())
                    {
                        bridge.refresh_viewing_journal();
                    }
                } else {
                    let mut state = bridge.shared.lock().unwrap();
                    state.viewing_journal.loaded = false;
                    state.viewing_journal.error = true;
                    state.viewing_journal.retry_at = Some(Instant::now() + LOAD_RETRY_DELAY);
                }
            });
        });
    }

    pub(super) fn setup_viewing_journal(&self) {
        let Some(app) = self.app() else { return };
        let journal = app.global::<JournalBridge>();
        let bridge = self.clone();
        journal.on_opened(move || bridge.refresh_viewing_journal());
        let bridge = self.clone();
        journal.on_select_year(move |year| {
            let Some(app) = bridge.app() else { return };
            let journal = app.global::<JournalBridge>();
            journal.set_year(if year == 0 {
                0
            } else {
                year.clamp(journal.get_oldest_year(), journal.get_current_year())
            });
            bridge.refresh_viewing_journal();
        });
    }

    /// Called after a native player tick, before the playback intent is retired.
    pub(super) fn note_viewing_journal(&self) {
        let Some(app) = self.app() else { return };
        if !app.get_player_open() {
            self.finish_viewing_journal();
            return;
        }
        if !app.get_playback_started() {
            return;
        }
        let Some(media) = self.current_playback_identity() else {
            return;
        };
        let readiness = self.player.playback_readiness();
        self.note_journal_observation(
            media,
            f64::from(app.get_position()),
            f64::from(app.get_duration()),
            !app.get_is_paused() && !readiness.buffering && !readiness.ended,
            f64::from(self.player.effective_playback_speed()),
        );
    }

    /// Playback backends supply actual observations without fabricating native-player state.
    pub(super) fn note_journal_observation(
        &self,
        media: nova_playback_api::MediaIdentity,
        position: f64,
        duration: f64,
        playing: bool,
        speed: f64,
    ) {
        if self.last_time_replay_active() {
            return;
        }
        let replay = self.last_time_playback_evidence(duration);
        let new_completion = self.shared.lock().unwrap().viewing_journal.observe(
            media.clone(),
            profiles::ProfileStore::capture(),
            position,
            duration,
            playing,
            speed,
        );
        if let Some(replay) = replay {
            let mut state = self.shared.lock().unwrap();
            if let Some(active) = state.viewing_journal.active.as_mut() {
                active.row.replay = Some(replay);
            }
        }
        if new_completion {
            // Metadata I/O stays outside Shared's lock. The ordinary progress
            // tick runs after this observer, so project only this newly observed
            // completion while checking the same title-completion rule as Library.
            let episodes =
                read_episodes_cache_for(&media.media_type, &media.title_id).unwrap_or_default();
            let mut state = self.shared.lock().unwrap();
            let is_rewatch = state
                .viewing_journal
                .active
                .as_ref()
                .is_some_and(|active| active.row.rewatch);
            if completed_title_run(&media, &episodes, &state.progress, today_days(), is_rewatch) {
                let tracker = &mut state.viewing_journal;
                if let Some(active) = tracker.active.as_mut() {
                    active.row.title_completed_at = active.row.completed_at;
                    tracker
                        .rows
                        .insert(active.row.id.clone(), active.row.clone());
                    persist(active.owner.clone(), active.row.clone());
                }
            }
        }
    }

    pub(super) fn finish_viewing_journal(&self) {
        self.shared
            .lock()
            .unwrap()
            .viewing_journal
            .finish(now_secs());
    }

    pub(super) fn refresh_viewing_journal(&self) {
        if profile_policy::entry_blocked() {
            return;
        }
        let Some(app) = self.app() else { return };
        let journal = app.global::<JournalBridge>();
        let now_year = year_at(now_secs());
        journal.set_current_year(now_year);
        let mut state = self.shared.lock().unwrap();
        let tracker = &mut state.viewing_journal;
        // Opening the journal explicitly retries a failed initial scan; normal
        // playback ticks retain the backoff instead of hammering storage.
        tracker.retry_at = None;
        tracker.ensure_loaded(&profiles::ProfileStore::capture());
        let oldest = tracker
            .rows
            .values()
            .map(|row| year_at(row.started_at))
            .min()
            .unwrap_or(now_year)
            .min(now_year);
        journal.set_oldest_year(oldest);
        let year = journal.get_year();
        let summary = summarize(
            tracker.rows.values().filter(|r| {
                profile_policy::allows_title(
                    &profiles::ProfileStore::capture(),
                    &r.media_type,
                    &r.title_id,
                )
            }),
            year,
        );
        journal.set_error(if tracker.error {
            text::tr(
                "Some viewing sessions could not be read. Your saved records have been preserved.",
            )
            .into()
        } else {
            SharedString::default()
        });
        journal.set_watched(text::journal_duration(summary.seconds).into());
        journal.set_session_count(summary.sessions.len().min(i32::MAX as usize) as i32);
        journal.set_title_count(summary.titles.len().min(i32::MAX as usize) as i32);
        journal.set_completion_count(summary.completions.min(i32::MAX as usize) as i32);
        journal.set_rewatch_count(summary.rewatches.min(i32::MAX as usize) as i32);
        journal.set_active_days(summary.active_days.min(i32::MAX as usize) as i32);
        journal.set_sessions(
            Rc::new(VecModel::from(
                summary
                    .sessions
                    .into_iter()
                    .take(100)
                    .map(|row| JournalSessionRow {
                        id: row.id.clone().into(),
                        title: row.title.clone().into(),
                        episode: row.episode.clone().into(),
                        date: text::journal_date(row.started_at, true).into(),
                        watched: text::journal_duration(row.seconds_in(year)).into(),
                        completed: row
                            .completed_at
                            .is_some_and(|at| year == 0 || year_at(at) == year),
                        rewatch: row.rewatch,
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        journal.set_titles(
            Rc::new(VecModel::from(
                summary
                    .titles
                    .into_iter()
                    .take(50)
                    .map(|row| JournalTitleRow {
                        title: row.title.into(),
                        is_movie: row.is_movie,
                        first_date: text::journal_date(row.first, false).into(),
                        finished_date: row
                            .finished
                            .map(|at| text::journal_date(at, false))
                            .unwrap_or_default()
                            .into(),
                        watched: text::journal_duration(row.seconds).into(),
                        sessions: row.sessions.min(i32::MAX as usize) as i32,
                        rewatches: row.rewatches.min(i32::MAX as usize) as i32,
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        let max_month = summary
            .months
            .iter()
            .copied()
            .fold(0.0_f64, f64::max)
            .max(1.0);
        journal.set_months(
            Rc::new(VecModel::from(
                summary
                    .months
                    .into_iter()
                    .enumerate()
                    .map(|(index, seconds)| JournalMonthRow {
                        label: format!("{:02}", index + 1).into(),
                        fraction: (seconds / max_month) as f32,
                        watched: text::journal_duration(seconds).into(),
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
    }
}

/// A session finishing an episode is weaker evidence than finishing a title.
/// Unknown metadata never dates a series finish. Future releases remain outside
/// the available-episode run, matching the Library's existing completion rule.
fn completed_title_run(
    media: &nova_playback_api::MediaIdentity,
    episodes: &[Video],
    progress: &HashMap<String, EpisodeProgress>,
    today: i64,
    is_rewatch: bool,
) -> bool {
    // Replaying one already-watched episode does not finish another series
    // run. Existing watched flags cannot establish full-series rewatch coverage.
    if is_rewatch && media.media_type != "movie" {
        return false;
    }
    if media.media_type != "movie"
        && !episodes
            .iter()
            .any(|episode| episode.id == media.episode_id)
    {
        return false;
    }
    let mut projected = progress.clone();
    let current = projected
        .entry(progress_map_key(&media.title_id, &media.episode_id))
        .or_default();
    current.series_id = media.title_id.clone();
    current.episode_id = media.episode_id.clone();
    current.watched = true;
    library_title_completed(
        &media.media_type,
        &media.title_id,
        episodes,
        &projected,
        today,
    )
}

#[derive(Default)]
struct TitleSummary {
    title: String,
    is_movie: bool,
    first: u64,
    finished: Option<u64>,
    seconds: f64,
    sessions: usize,
    rewatches: usize,
}

struct Summary<'a> {
    seconds: f64,
    sessions: Vec<&'a ViewingSession>,
    titles: Vec<TitleSummary>,
    months: [f64; 12],
    completions: usize,
    rewatches: usize,
    active_days: usize,
}

fn summarize<'a>(rows: impl Iterator<Item = &'a ViewingSession>, year: i32) -> Summary<'a> {
    let mut summary = Summary {
        seconds: 0.0,
        sessions: Vec::new(),
        titles: Vec::new(),
        months: [0.0; 12],
        completions: 0,
        rewatches: 0,
        active_days: 0,
    };
    let mut titles: BTreeMap<(&str, &str), TitleSummary> = BTreeMap::new();
    let mut days = HashSet::new();
    let mut first_dates: BTreeMap<(&str, &str), u64> = BTreeMap::new();
    for row in rows {
        let first = first_dates
            .entry((&row.media_type, &row.title_id))
            .or_insert(row.started_at);
        *first = (*first).min(row.started_at);
        let seconds = row.seconds_in(year);
        if seconds <= 0.0 {
            continue;
        }
        summary.seconds += seconds;
        summary.sessions.push(row);
        // Dates describe observed sessions; a resumed title does not acquire a
        // fabricated historical start date from old episode progress.
        days.extend(
            row.days
                .keys()
                .copied()
                .filter(|day| year == 0 || year_at(day * 86400) == year),
        );
        let completed = row
            .completed_at
            .filter(|at| year == 0 || year_at(*at) == year);
        summary.completions += usize::from(completed.is_some());
        summary.rewatches += usize::from(completed.is_some() && row.rewatch);
        let title = titles
            .entry((&row.media_type, &row.title_id))
            .or_insert_with(|| TitleSummary {
                title: row.title.clone(),
                is_movie: row.media_type == "movie",
                first: row.started_at,
                ..Default::default()
            });
        title.first = title.first.min(row.started_at);
        title.finished = title.finished.max(
            row.title_completed_at
                .filter(|at| year == 0 || year_at(*at) == year),
        );
        title.seconds += seconds;
        title.sessions += 1;
        title.rewatches += usize::from(completed.is_some() && row.rewatch);
        for (month, seconds) in &row.months {
            if (year == 0 || month.starts_with(&format!("{year:04}-")))
                && let Some(month) = month.get(5..).and_then(|month| month.parse::<usize>().ok())
                && (1..=12).contains(&month)
            {
                summary.months[month - 1] += seconds;
            }
        }
    }
    summary
        .sessions
        .sort_by_key(|row| std::cmp::Reverse(row.started_at));
    for (key, title) in &mut titles {
        title.first = first_dates[key];
    }
    summary.titles = titles.into_values().collect();
    summary.titles.sort_by(|a, b| {
        b.seconds
            .total_cmp(&a.seconds)
            .then_with(|| a.title.cmp(&b.title))
    });
    summary.active_days = days.len();
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(
        id: &str,
        title: &str,
        at: u64,
        seconds: f64,
        completed: bool,
        rewatch: bool,
    ) -> ViewingSession {
        ViewingSession {
            id: id.into(),
            media_type: "series".into(),
            title_id: title.into(),
            episode_id: "episode".into(),
            title: title.into(),
            episode: "Episode 1".into(),
            started_at: at,
            updated_at: at,
            ended_at: Some(at),
            watched_seconds: seconds,
            months: BTreeMap::from([(month_at(at), seconds)]),
            days: BTreeMap::from([(at / 86400, seconds)]),
            completed_at: completed.then_some(at),
            title_completed_at: completed.then_some(at),
            rewatch,
            replay: None,
            viewing_run_start: false,
        }
    }

    #[test]
    fn last_time_late_open_then_restart_or_recovered_history_cannot_leak_a_prior_run() {
        let media = nova_playback_api::MediaIdentity {
            media_type: "series".into(),
            title_id: "Show".into(),
            episode_id: "show:1:1".into(),
            ..Default::default()
        };
        for recovered in [false, true] {
            let mut first = session("prior-first", "Show", 20, 600.0, true, true);
            first.episode_id = "show:1:1".into();
            first.viewing_run_start = true;
            let mut later = session("prior-later", "Show", 30, 600.0, true, true);
            later.episode_id = "show:1:9".into();
            let mut tracker = JournalTracker {
                loaded: true,
                rows: BTreeMap::from([(first.id.clone(), first), (later.id.clone(), later)]),
                ..Default::default()
            };
            tracker.observe(
                media.clone(),
                profiles::ProfileStore::capture(),
                590.0,
                600.0,
                true,
                1.0,
            );
            assert!(!tracker.active.as_ref().unwrap().row.rewatch);
            if recovered {
                let active = tracker.active.as_mut().unwrap();
                active.history_known = false;
                active.started_position = 0.0;
                tracker.reconcile_active_history();
            } else {
                tracker.observe(
                    media.clone(),
                    profiles::ProfileStore::capture(),
                    0.0,
                    600.0,
                    true,
                    1.0,
                );
            }
            let active = tracker.active.as_mut().unwrap();
            assert!(active.row.rewatch);
            assert!(active.row.viewing_run_start);
            active.last_position = 0.0;
            active.last_tick = Instant::now() - Duration::from_secs(1);
            active.was_playing = true;
            active.saved_seconds = 0.0;
            tracker.observe(
                media.clone(),
                profiles::ProfileStore::capture(),
                1.0,
                600.0,
                true,
                1.0,
            );
            let facts = project_last_time(tracker.rows.values().collect(), false, Some(now_secs()));
            assert!(
                facts.completed.is_none(),
                "a partial restarted run must not disclose an old later episode; recovered={recovered}"
            );
        }
    }

    #[test]
    fn last_time_date_uses_playback_days_instead_of_later_stop_or_invalid_allocations() {
        let mut row = session("paused", "Show", 100 * 86400 + 100, 30.0, false, false);
        row.updated_at = 102 * 86400;
        assert_eq!(last_playback_date(&row, 102 * 86400), Some(100 * 86400));
        row.days.insert(101, 20.0);
        row.days.insert(102, f64::NAN);
        row.days.insert(103, 10.0);
        row.days.insert(99, 40.0);
        assert_eq!(last_playback_date(&row, 102 * 86400), Some(101 * 86400));
        row.days.clear();
        assert_eq!(
            last_playback_date(&row, 102 * 86400),
            None,
            "a Stop timestamp cannot invent a playback date"
        );
        row.completed_at = Some(101 * 86400 + 200);
        assert_eq!(last_playback_date(&row, 102 * 86400), Some(101 * 86400));
        row.completed_at = Some(103 * 86400);
        assert_eq!(last_playback_date(&row, 102 * 86400), None);
    }

    #[test]
    fn last_time_uses_latest_completion_chronology_and_excludes_unknown_rewatch_runs() {
        let mut higher = session("higher", "Show", 10, 600.0, true, false);
        higher.episode_id = "show:1:9".into();
        let mut later = session("later", "Show", 20, 600.0, true, false);
        later.episode_id = "show:1:2".into();
        let mut partial = session("partial", "Show", 30, 100.0, false, false);
        partial.episode_id = "show:1:3".into();
        let facts = project_last_time(vec![&higher, &partial, &later], false, Some(30));
        assert_eq!(facts.last_at, Some(30));
        assert_eq!(facts.completed.unwrap().episode_id, "show:1:2");
        partial.rewatch = true;
        assert!(
            project_last_time(vec![&higher, &later, &partial], false, Some(30))
                .completed
                .is_none(),
            "unknown historical rewatch must not leak an old run's synopsis"
        );
        partial.viewing_run_start = true;
        assert!(
            project_last_time(vec![&higher, &later, &partial], false, Some(30))
                .completed
                .is_none()
        );
        let mut new_run = session("new-run", "Show", 40, 600.0, true, true);
        new_run.episode_id = "show:1:1".into();
        let facts = project_last_time(vec![&higher, &later, &partial, &new_run], false, Some(40));
        assert_eq!(facts.completed.unwrap().episode_id, "show:1:1");
        assert!(
            project_last_time(vec![&new_run], true, Some(40))
                .completed
                .is_none(),
            "unreadable rows may hide a later run"
        );
    }

    #[test]
    fn last_time_rewatch_successors_share_a_run_but_a_second_restart_starts_another() {
        let mut first = session("first", "Show", 20, 600.0, true, true);
        first.episode_id = "show:1:1".into();
        first.viewing_run_start = true;
        let mut media = nova_playback_api::MediaIdentity {
            media_type: "series".into(),
            title_id: "Show".into(),
            episode_id: "show:1:2".into(),
            ..Default::default()
        };
        assert!(!starts_viewing_run([&first].into_iter(), &media));
        media.episode_id = first.episode_id.clone();
        assert!(starts_viewing_run([&first].into_iter(), &media));
        first.completed_at = None;
        assert!(
            !starts_viewing_run([&first].into_iter(), &media),
            "resuming an unfinished episode is the same run"
        );
    }

    #[test]
    fn failed_initial_scan_retries_and_merges_history_with_sessions_observed_during_failure() {
        let now = Instant::now();
        let mut tracker = JournalTracker::default();
        tracker.try_load(now, || Err(()));
        assert!(!tracker.loaded);
        assert!(tracker.error);
        let mut observed = session("new", "Show", 20, 45.0, false, false);
        observed.ended_at = None;
        tracker.rows.insert(observed.id.clone(), observed.clone());
        tracker.try_load(now + Duration::from_secs(1), || {
            panic!("failed reads must back off between playback ticks")
        });
        let old = session("old", "Show", 10, 600.0, true, false);
        let stale_checkpoint = session("new", "Show", 20, 15.0, false, false);
        tracker.try_load(now + LOAD_RETRY_DELAY, || {
            Ok((
                BTreeMap::from([
                    (old.id.clone(), old),
                    (stale_checkpoint.id.clone(), stale_checkpoint),
                ]),
                false,
            ))
        });
        assert!(tracker.loaded);
        assert!(!tracker.error);
        assert_eq!(tracker.rows.len(), 2);
        assert_eq!(tracker.rows["new"].watched_seconds, 45.0);
        assert!(tracker.rows["new"].ended_at.is_none());
        assert!(tracker.rows["old"].completed_at.is_some());
        let media = nova_playback_api::MediaIdentity {
            media_type: "series".into(),
            title_id: "Show".into(),
            episode_id: "episode".into(),
            title: "Show".into(),
            episode: "Episode 1".into(),
        };
        assert_eq!(
            rewatch_state(tracker.rows.values(), &media, 0.0, 600.0),
            (true, true)
        );
        tracker.try_load(now + LOAD_RETRY_DELAY, || {
            panic!("successful scans stay loaded")
        });
    }

    #[test]
    fn recovered_history_immediately_reclassifies_active_session_from_original_start_position() {
        let now = Instant::now();
        for (started_position, expected_rewatch) in [(0.0, true), (590.0, false)] {
            let mut tracker = JournalTracker::default();
            tracker.try_load(now, || Err(()));
            let mut observed = session("new", "Show", 20, 45.0, false, false);
            observed.ended_at = None;
            tracker.rows.insert(observed.id.clone(), observed.clone());
            tracker.active = Some(ActiveSession {
                row: observed,
                owner: profiles::ProfileStore::capture(),
                last_tick: now,
                // Playback has moved beyond the restart threshold by recovery.
                last_position: 300.0,
                was_playing: false,
                completion_eligible: false,
                history_known: false,
                started_position,
                media_duration: 600.0,
                saved_seconds: 30.0,
            });
            let old = session("old", "Show", 10, 600.0, true, false);
            tracker.try_load(now + LOAD_RETRY_DELAY, || {
                Ok((BTreeMap::from([(old.id.clone(), old)]), false))
            });
            let active = tracker.active.as_ref().unwrap();
            assert!(active.history_known);
            assert_eq!(active.row.rewatch, expected_rewatch);
            assert_eq!(active.completion_eligible, expected_rewatch);
            assert_eq!(active.row.watched_seconds, 45.0);
            assert_eq!(active.last_position, 300.0);
            assert_eq!(active.saved_seconds, 30.0);
            assert_eq!(tracker.rows["new"].rewatch, expected_rewatch);
            assert_eq!(tracker.rows["new"].watched_seconds, 45.0);
            assert!(tracker.rows["old"].completed_at.is_some());
        }
    }

    #[test]
    fn seeks_pauses_stalls_and_background_gaps_do_not_inflate_watch_time() {
        assert_eq!(watched_interval(1.0, 10.0, 11.0, true, 1.0), 1.0);
        assert_eq!(watched_interval(0.25, 10.0, 11.0, true, 1.0), 0.0);
        assert_eq!(watched_interval(1.0, 10.0, 10.5, true, 0.5), 1.0);
        assert_eq!(watched_interval(1.0, 10.0, 12.0, true, 2.0), 1.0);
        assert_eq!(watched_interval(1.0, 10.0, 900.0, true, 1.0), 0.0);
        assert_eq!(watched_interval(1.0, 10.0, 5.0, true, 1.0), 0.0);
        assert_eq!(watched_interval(1.0, 10.0, 10.0, true, 1.0), 0.0);
        assert_eq!(watched_interval(1.0, 10.0, 11.0, false, 1.0), 0.0);
        assert_eq!(watched_interval(60.0, 10.0, 70.0, true, 1.0), 0.0);
        assert_eq!(watched_interval(1.0, 10.0, f64::NAN, true, 1.0), 0.0);
    }

    #[test]
    fn recap_filters_year_and_counts_completed_rewatches_without_historical_progress() {
        let jan = (super::super::home::days_from_civil(2025, 1, 2) * 86400) as u64;
        let next = (super::super::home::days_from_civil(2026, 2, 3) * 86400) as u64;
        let rows = [
            session("a", "Show", jan, 1200.0, true, false),
            session("b", "Show", next, 1500.0, true, true),
            session("c", "Film", next, 300.0, false, false),
        ];
        let yearly = summarize(rows.iter(), 2026);
        assert_eq!(yearly.seconds, 1800.0);
        assert_eq!(yearly.sessions.len(), 2);
        assert_eq!(yearly.titles.len(), 2);
        assert_eq!(yearly.titles[0].first, jan);
        assert_eq!(yearly.completions, 1);
        assert_eq!(yearly.rewatches, 1);
        assert_eq!(yearly.months[1], 1800.0);
        assert_eq!(yearly.active_days, 1);
        let all = summarize(rows.iter(), 0);
        assert_eq!(all.seconds, 3000.0);
        assert_eq!(all.titles[0].first, jan);
        assert_eq!(all.titles[0].finished, Some(next));
        assert_eq!(summarize(rows.iter(), 2024).sessions.len(), 0);
    }

    #[test]
    fn sessions_crossing_new_year_allocate_time_to_both_recaps_and_round_trip() {
        let at = (super::super::home::days_from_civil(2025, 12, 31) * 86400) as u64;
        let mut row = session("same-id", "Film", at, 120.0, false, false);
        row.days = BTreeMap::from([(at / 86400, 30.0), (at / 86400 + 1, 90.0)]);
        row.months = BTreeMap::from([("2025-12".into(), 30.0), ("2026-01".into(), 90.0)]);
        let restored: ViewingSession =
            serde_json::from_str(&serde_json::to_string(&row).unwrap()).unwrap();
        assert_eq!(restored.id, "same-id");
        assert_eq!(restored.seconds_in(2025), 30.0);
        assert_eq!(restored.seconds_in(2026), 90.0);
        assert_eq!(restored.seconds_in(0), 120.0);
        assert_eq!(summarize(std::iter::once(&restored), 2026).active_days, 1);
    }

    #[test]
    fn rewatches_require_a_restart_and_survive_resuming_an_unfinished_rewatch() {
        let media = nova_playback_api::MediaIdentity {
            media_type: "series".into(),
            title_id: "Show".into(),
            episode_id: "episode".into(),
            title: "Show".into(),
            episode: "Episode 1".into(),
        };
        let completed = session("original", "Show", 10, 600.0, true, false);
        assert_eq!(
            rewatch_state(std::iter::empty(), &media, 500.0, 600.0),
            (false, true)
        );
        assert_eq!(
            rewatch_state(std::iter::once(&completed), &media, 590.0, 600.0),
            (false, false)
        );
        assert_eq!(
            rewatch_state(std::iter::once(&completed), &media, 0.0, 600.0),
            (true, true)
        );
        let interrupted = session("restarted", "Show", 20, 300.0, false, true);
        assert_eq!(
            rewatch_state([&completed, &interrupted].into_iter(), &media, 400.0, 600.0),
            (true, true)
        );
        let later_completion = session("finished-rewatch", "Show", 30, 300.0, true, true);
        assert_eq!(
            rewatch_state(
                [&completed, &interrupted, &later_completion].into_iter(),
                &media,
                590.0,
                600.0
            ),
            (false, false)
        );
    }

    #[test]
    fn a_finished_episode_dates_the_title_only_when_known_released_episodes_are_complete() {
        let media = nova_playback_api::MediaIdentity {
            media_type: "series".into(),
            title_id: "show".into(),
            episode_id: "e1".into(),
            title: "Show".into(),
            episode: "Episode 1".into(),
        };
        let episodes = [
            Video {
                id: "e1".into(),
                released: Some("2025-01-01".into()),
                ..Default::default()
            },
            Video {
                id: "e2".into(),
                released: Some("2025-01-02".into()),
                ..Default::default()
            },
        ];
        let today = super::super::home::days_from_civil(2026, 1, 1);
        let mut progress = HashMap::new();
        assert!(!completed_title_run(&media, &[], &progress, today, false));
        assert!(!completed_title_run(
            &media, &episodes, &progress, today, false
        ));
        progress.insert(
            progress_map_key("show", "e2"),
            EpisodeProgress {
                series_id: "show".into(),
                episode_id: "e2".into(),
                watched: true,
                ..Default::default()
            },
        );
        assert!(completed_title_run(
            &media, &episodes, &progress, today, false
        ));
        assert!(!completed_title_run(
            &media, &episodes, &progress, today, true
        ));
        let mut unknown = media.clone();
        unknown.episode_id = "unlisted".into();
        assert!(!completed_title_run(
            &unknown, &episodes, &progress, today, false
        ));
        let mut episode_only = session("session", "show", 10, 600.0, true, false);
        episode_only.title_completed_at = None;
        assert_eq!(
            summarize(std::iter::once(&episode_only), 0).titles[0].finished,
            None
        );
        let movie = nova_playback_api::MediaIdentity {
            media_type: "movie".into(),
            title_id: "film".into(),
            episode_id: "film".into(),
            title: "Film".into(),
            episode: String::new(),
        };
        assert!(completed_title_run(
            &movie,
            &[],
            &HashMap::new(),
            today,
            true
        ));
    }

    #[test]
    fn media_types_keep_same_native_id_as_separate_titles() {
        let mut rows = [
            session("a", "id", 1, 30.0, false, false),
            session("b", "id", 2, 40.0, false, false),
        ];
        rows[1].media_type = "movie".into();
        assert_eq!(summarize(rows.iter(), 0).titles.len(), 2);
        assert_ne!(unique_id(), unique_id());
        assert_eq!(text::journal_duration(3723.0), "1:02:03");
        assert_eq!(text::journal_date(0, true), "1970-01-01 00:00 UTC");
    }
}

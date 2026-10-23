//! Profile-owned release subscriptions, local notification receipts and portable
//! RFC 5545 snapshot ledgers. Calendar metadata remains in the ordinary cache.
use super::*;
use chrono::{Local, NaiveDate, Timelike, Utc};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) const KEY: &str = "release_subscriptions:v1";
pub(super) const DOMAIN: &str = "release_subscriptions";
const DELIVERY_KEY: &str = "release_delivery:v1";
const EXPORT_KEY: &str = "calendar_export:v1";
static DELIVERY_SERIAL: Mutex<()> = Mutex::new(());
static DELIVERY_RUNNING: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
thread_local! { static BEFORE_DELIVERY_LOCK: std::cell::RefCell<Option<std::sync::mpsc::Sender<()>>> = const { std::cell::RefCell::new(None) }; }
pub(super) fn delivery_guard() -> std::sync::MutexGuard<'static, ()> {
    #[cfg(test)]
    BEFORE_DELIVERY_LOCK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            let _ = hook.send(());
        }
    });
    DELIVERY_SERIAL.lock().unwrap()
}
fn mutate_delivery(
    owner: &profiles::ProfileStore,
    change: impl FnOnce(&mut Delivery) -> Result<(), String>,
) -> Result<(), String> {
    let _serial = delivery_guard();
    let mut state: Delivery = load(owner, DELIVERY_KEY)?;
    change(&mut state)?;
    owner
        .write(
            DELIVERY_KEY,
            &serde_json::to_string(&state).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
}
#[cfg(target_os = "android")]
static PENDING_TAP: Mutex<Option<(String, String, bool)>> = Mutex::new(None);
#[cfg(target_os = "android")]
pub(super) fn queue_notification_tap(profile: String, event: String) {
    if profiles::valid_id(&profile)
        && serde_json::from_str::<(String, String, String)>(&event).is_ok()
    {
        *PENDING_TAP.lock().unwrap() = Some((profile, event, false));
    }
}
pub(super) fn cancel_pending_notification_tap() {
    #[cfg(target_os = "android")]
    {
        PENDING_TAP.lock().unwrap().take();
    }
}
/// Import restores subscriptions, never a device's notification opt-in. The
/// caller stages this local row in its atomic import/undo receipt transaction.
pub(super) fn import_delivery_edit(
    owner: &profiles::ProfileStore,
) -> Result<(String, Option<String>), String> {
    let _serial = delivery_guard();
    let mut state: Delivery = load(owner, DELIVERY_KEY)?;
    state.enabled = false;
    state.visible.clear();
    Ok((
        DELIVERY_KEY.into(),
        Some(serde_json::to_string(&state).map_err(|e| e.to_string())?),
    ))
}
pub(super) fn cancel_profile_notifications(profile: &str) {
    #[cfg(target_os = "android")]
    release_reminders_android::notify(profile, "", "", false, "", "[]");
    #[cfg(not(target_os = "android"))]
    let _ = profile;
}
#[derive(Default)]
pub(super) struct State {
    owner: String,
    selected: HashSet<String>,
    pending: Option<(ExportLedger, String, String, String)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Subscription {
    pub type_: String,
    pub title_id: String,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}
pub(super) fn title_key(type_: &str, id: &str) -> String {
    serde_json::to_string(&(type_, id)).expect("title identity")
}
fn subscriptions(owner: &profiles::ProfileStore) -> Result<BTreeMap<String, Subscription>, String> {
    load(owner, KEY)
}
fn load<T: serde::de::DeserializeOwned + Default>(
    owner: &profiles::ProfileStore,
    key: &str,
) -> Result<T, String> {
    owner
        .read(key)
        .map_err(|e| e.to_string())?
        .map(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
        .unwrap_or_else(|| Ok(T::default()))
}
pub(super) fn subscribed(owner: &profiles::ProfileStore, type_: &str, id: &str) -> bool {
    subscriptions(owner).is_ok_and(|s| s.contains_key(&title_key(type_, id)))
}
pub(super) fn note_metadata_refresh(owner: &profiles::ProfileStore, result: Result<(), String>) {
    let _serial = DELIVERY_SERIAL.lock().unwrap();
    if let Ok(mut state) = load::<Delivery>(owner, DELIVERY_KEY) {
        match result {
            Ok(()) => {
                state.last_refreshed = Some(Utc::now().timestamp());
                state.refresh_error = None;
            }
            Err(error) => state.refresh_error = Some(error),
        }
        if let Err(error) = owner.write(DELIVERY_KEY, &serde_json::to_string(&state).unwrap()) {
            storage::report(error);
        }
    }
}
#[cfg(target_os = "android")]
pub(super) fn refresh_owner_metadata(
    owner: &profiles::ProfileStore,
    cancel: &AtomicBool,
    started: std::time::Instant,
) {
    let Ok(state) = load::<Delivery>(owner, DELIVERY_KEY) else {
        return;
    };
    if !state.enabled {
        return;
    }
    let Ok(subs) = subscriptions(owner) else {
        return;
    };
    let entries: Vec<LibraryEntry> = load(owner, "library").unwrap_or_default();
    for sub in subs
        .values()
        .filter(|s| {
            entries.iter().any(|e| {
                e.id == s.title_id && e.type_ == s.type_ && e.watch_status == WatchStatus::Auto
            })
        })
        .take(4)
    {
        if cancel.load(Ordering::Acquire) || started.elapsed() > Duration::from_secs(50) {
            break;
        }
        if !profile_policy::allows_title(owner, &sub.type_, &sub.title_id) {
            continue;
        }
        note_metadata_refresh(
            owner,
            headless_profile::refresh_metadata(owner, &sub.type_, &sub.title_id, cancel),
        );
    }
}
pub(super) fn records(raw: &str) -> nova_sync::Result<Vec<(String, String, u64)>> {
    let rows: BTreeMap<String, Subscription> = serde_json::from_str(raw)?;
    rows.into_iter()
        .map(|(key, value)| {
            if value.type_.is_empty()
                || value.title_id.is_empty()
                || key != title_key(&value.type_, &value.title_id)
            {
                return Err(storage::Error::new(
                    storage::ErrorKind::Schema,
                    "invalid release subscription identity",
                )
                .into());
            }
            Ok((key, serde_json::to_string(&value)?, 0))
        })
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum ReleaseTime {
    Day(NaiveDate),
    Exact(i64),
}
impl ReleaseTime {
    fn parse(raw: &str) -> Option<Self> {
        if let Ok(exact) = chrono::DateTime::parse_from_rfc3339(raw) {
            return Some(Self::Exact(exact.timestamp()));
        }
        // A naive timestamp supplies a calendar date, never an inferred zone.
        if raw.len() == 10 {
            return NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .ok()
                .map(Self::Day);
        }
        chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
            .ok()
            .map(|naive| Self::Day(naive.date()))
    }
    fn date(&self) -> NaiveDate {
        match self {
            Self::Day(day) => *day,
            Self::Exact(at) => chrono::DateTime::from_timestamp(*at, 0)
                .expect("parsed timestamp")
                .with_timezone(&Local)
                .date_naive(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Event {
    key: String,
    type_: String,
    title_id: String,
    episode_id: String,
    summary: String,
    description: String,
    time: ReleaseTime,
    watched: bool,
    #[serde(default)]
    aliases: Vec<String>,
}
fn proven_aliases(video: &Video) -> Vec<String> {
    let mut aliases = confirmed_episode_stream_ids(video);
    aliases.push(video.id.clone());
    aliases.sort();
    aliases.dedup();
    let mut counts = HashMap::new();
    for id in &aliases {
        if let Some(namespace) = media_identity::canonical_episode_namespace(id) {
            *counts.entry(namespace).or_insert(0) += 1;
        }
    }
    aliases.retain(|id| {
        media_identity::canonical_episode_namespace(id)
            .is_some_and(|namespace| counts.get(namespace) == Some(&1))
    });
    aliases
}
fn events(
    owner: &profiles::ProfileStore,
    only_subscribed: bool,
    query: &str,
    selected: Option<&HashSet<String>>,
) -> Result<(Vec<Event>, usize), String> {
    let titles: Vec<LibraryEntry> = load(owner, "library")?;
    let progress: HashMap<String, EpisodeProgress> = load(owner, EPISODE_PROGRESS_KEY)?;
    let subs = subscriptions(owner)?;
    let mut result = Vec::new();
    let mut undated = 0;
    for title in titles {
        if title.type_ == "movie"
            || matches!(
                title.watch_status,
                WatchStatus::OnHold | WatchStatus::Dropped
            )
            || !title.name.to_lowercase().contains(&query.to_lowercase())
            || only_subscribed && !subs.contains_key(&title_key(&title.type_, &title.id))
            || selected
                .is_some_and(|selection| !selection.contains(&title_key(&title.type_, &title.id)))
            || !profile_policy::allows_title(owner, &title.type_, &title.id)
        {
            continue;
        }
        let videos: Vec<Video> = owner
            .read(&episodes_key(&title.type_, &title.id))
            .map_err(|e| e.to_string())?
            .map(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
            .transpose()?
            .unwrap_or_default();
        for video in videos {
            let Some(time) = video.released.as_deref().and_then(ReleaseTime::parse) else {
                undated += 1;
                continue;
            };
            let key = serde_json::to_string(&(&title.type_, &title.id, &video.id))
                .expect("event identity");
            result.push(Event {
                key,
                type_: title.type_.clone(),
                title_id: title.id.clone(),
                episode_id: video.id.clone(),
                summary: format!("{} — {}", title.name, video.label()),
                description: video.overview.clone().unwrap_or_default(),
                time,
                aliases: proven_aliases(&video)
                    .into_iter()
                    .map(|id| serde_json::to_string(&(&title.type_, id)).unwrap())
                    .collect(),
                watched: std::iter::once(video.id.clone())
                    .chain(proven_aliases(&video))
                    .any(|id| {
                        progress
                            .get(&progress_map_key(&title.id, &id))
                            .is_some_and(|p| p.watched)
                    }),
            });
        }
    }
    let mut seen = HashSet::new();
    result.retain(|event| seen.insert(event.key.clone()));
    let mut claims = HashMap::<String, usize>::new();
    for event in &result {
        for alias in &event.aliases {
            *claims.entry(alias.clone()).or_default() += 1;
        }
    }
    for event in &mut result {
        event.aliases.retain(|alias| claims.get(alias) == Some(&1));
    }
    result.sort_by(|a, b| (&a.time.date(), &a.key).cmp(&(&b.time.date(), &b.key)));
    result.dedup_by(|a, b| a.key == b.key);
    Ok((result, undated))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Delivery {
    enabled: bool,
    // Identity receipts deliberately exclude timezone/date revisions: travel,
    // clock corrections and schedule edits cannot repeat an episode alert.
    delivered: BTreeMap<String, i64>,
    proofs: BTreeMap<String, Vec<String>>,
    digest_day: Option<NaiveDate>,
    visible: Vec<Event>,
    dismissed: HashSet<String>,
    last_refreshed: Option<i64>,
    refresh_error: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}
#[cfg(any(target_os = "android", test))]
fn dismiss_native(state: &mut Delivery, keys: &[String]) {
    state.dismissed.extend(
        keys.iter()
            .filter(|key| state.delivered.contains_key(*key))
            .cloned(),
    );
}
#[cfg(any(target_os = "android", test))]
fn native_events(state: &Delivery) -> Vec<&Event> {
    state
        .visible
        .iter()
        .filter(|event| !state.dismissed.contains(&event.key))
        .collect()
}
#[cfg(target_os = "android")]
pub(super) fn acknowledge_dismissal(
    owner: &profiles::ProfileStore,
    keys: &[String],
) -> Result<(), String> {
    mutate_delivery(owner, |state| {
        dismiss_native(state, keys);
        Ok(())
    })
}
#[cfg(target_os = "android")]
pub(super) fn update_android_schedule(profiles: &[profiles::Profile]) {
    let enabled = profiles.iter().any(|profile| {
        load::<Delivery>(
            &profiles::ProfileStore::for_profile(&profile.id),
            DELIVERY_KEY,
        )
        .is_ok_and(|state| state.enabled)
    });
    release_reminders_android::configure(enabled);
}
fn reconcile_delivery(
    state: &mut Delivery,
    all: &[Event],
    now: i64,
    local_day: NaiveDate,
    local_seconds: u32,
) -> bool {
    state.visible.retain(|old| {
        all.iter().any(|e| e.key == old.key && !e.watched)
            && state
                .delivered
                .get(&old.key)
                .is_some_and(|at| now.saturating_sub(*at) <= 24 * 3600)
    });
    for visible in &mut state.visible {
        if let Some(current) = all.iter().find(|e| e.key == visible.key) {
            *visible = current.clone();
        }
    }
    if !state.enabled {
        return false;
    }
    // Yesterday is eligible only until its digest would be 24 hours overdue.
    let yesterday = local_day.pred_opt();
    let due: Vec<_> = all
        .iter()
        .filter(|e| {
            !e.watched
                && !state.delivered.contains_key(&e.key)
                && !state
                    .proofs
                    .values()
                    .any(|aliases| aliases.iter().any(|id| e.aliases.contains(id)))
                && (local_seconds >= 9 * 3600 && e.time.date() == local_day
                    || local_seconds < 9 * 3600 && Some(e.time.date()) == yesterday)
        })
        .cloned()
        .collect();
    if due.is_empty() {
        return false;
    }
    for event in &due {
        state.delivered.insert(event.key.clone(), now);
        state
            .proofs
            .insert(event.key.clone(), event.aliases.clone());
    }
    let alert = state.digest_day != Some(local_day);
    state.visible.extend(due);
    state.digest_day = Some(local_day);
    alert
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct ExportLedger {
    namespace: String,
    only_subscribed: bool,
    query: String,
    selected: Option<HashSet<String>>,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    entries: BTreeMap<String, Exported>,
    // Confirmed alias reconciliation can bind a new canonical key to a saved UID.
    aliases: BTreeMap<String, String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Exported {
    event: Event,
    uid: String,
    sequence: u32,
    modified: i64,
    cancelled: bool,
}
fn uid(namespace: &str, key: &str) -> String {
    let bytes = serde_json::to_vec(&("nova-calendar-v1", namespace, key)).expect("UID identity");
    format!("{}@nova", hex_digest(bytes))
}
fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\\n")
        .replace(';', "\\;")
        .replace(',', "\\,")
}
fn line(output: &mut String, value: &str) {
    let mut width = 0;
    for ch in value.chars() {
        if width + ch.len_utf8() > 75 {
            output.push_str("\r\n ");
            width = 1;
        }
        output.push(ch);
        width += ch.len_utf8();
    }
    output.push_str("\r\n");
}
fn stamp(at: i64) -> String {
    chrono::DateTime::<Utc>::from_timestamp(at, 0)
        .unwrap_or_default()
        .format("%Y%m%dT%H%M%SZ")
        .to_string()
}
fn export_ics(
    ledger: &mut ExportLedger,
    all: &[Event],
    start: NaiveDate,
    end: NaiveDate,
    now: i64,
) -> String {
    let mut output = String::new();
    for value in [
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        "PRODID:-//Nova//Release Calendar//EN",
        "CALSCALE:GREGORIAN",
        "METHOD:PUBLISH",
    ] {
        line(&mut output, value);
    }
    for event in all {
        if ledger.entries.contains_key(&event.key) || ledger.aliases.contains_key(&event.key) {
            continue;
        }
        let matching: Vec<_> = ledger
            .entries
            .iter()
            .filter(|(_, old)| {
                old.event
                    .aliases
                    .iter()
                    .any(|alias| event.aliases.contains(alias))
                    && all
                        .iter()
                        .filter(|other| other.aliases.iter().any(|a| event.aliases.contains(a)))
                        .count()
                        == 1
            })
            .map(|(key, _)| key.clone())
            .collect();
        if matching.len() == 1 {
            ledger
                .aliases
                .insert(event.key.clone(), matching[0].clone());
        }
    }
    let present: HashSet<_> = all
        .iter()
        .map(|e| {
            ledger
                .aliases
                .get(&e.key)
                .cloned()
                .unwrap_or_else(|| e.key.clone())
        })
        .collect();
    // Window changes don't cancel events outside the new window. Genuine
    // disappearance inside the saved window produces one durable cancellation.
    for (canonical, saved) in &mut ledger.entries {
        if !present.contains(canonical)
            && !saved.cancelled
            && saved.event.time.date() >= start
            && saved.event.time.date() <= end
        {
            saved.cancelled = true;
            saved.sequence = saved.sequence.saturating_add(1);
            saved.modified = now;
        }
    }
    let mut revised = HashSet::new();
    let eligible: Vec<_> = all
        .iter()
        .filter(|e| {
            e.time.date() >= start && e.time.date() <= end
                || ledger
                    .entries
                    .contains_key(ledger.aliases.get(&e.key).unwrap_or(&e.key))
        })
        .collect();
    for event in eligible {
        let canonical = ledger
            .aliases
            .get(&event.key)
            .cloned()
            .unwrap_or_else(|| event.key.clone());
        let entry = ledger
            .entries
            .entry(canonical.clone())
            .or_insert_with(|| Exported {
                event: event.clone(),
                uid: uid(&ledger.namespace, &canonical),
                sequence: 0,
                modified: now,
                cancelled: false,
            });
        // Watched state is delivery policy, not an external event revision.
        if (&event.summary, &event.description, &event.time)
            != (
                &entry.event.summary,
                &entry.event.description,
                &entry.event.time,
            )
            || entry.cancelled
        {
            entry.sequence = entry.sequence.saturating_add(1);
            entry.modified = now;
            entry.cancelled = false;
            revised.insert(canonical.clone());
        }
        entry.event = event.clone();
    }
    for (_, saved) in ledger.entries.iter().filter(|(key, e)| {
        e.event.time.date() >= start && e.event.time.date() <= end || revised.contains(*key)
    }) {
        line(&mut output, "BEGIN:VEVENT");
        for value in [
            format!("UID:{}", saved.uid),
            format!("DTSTAMP:{}", stamp(now)),
            format!("SEQUENCE:{}", saved.sequence),
            format!("LAST-MODIFIED:{}", stamp(saved.modified)),
            format!("SUMMARY:{}", escape(&saved.event.summary)),
            format!("DESCRIPTION:{}", escape(&saved.event.description)),
        ] {
            line(&mut output, &value);
        }
        match saved.event.time {
            ReleaseTime::Day(day) => {
                line(
                    &mut output,
                    &format!("DTSTART;VALUE=DATE:{}", day.format("%Y%m%d")),
                );
                line(
                    &mut output,
                    &format!(
                        "DTEND;VALUE=DATE:{}",
                        day.succ_opt().unwrap_or(day).format("%Y%m%d")
                    ),
                );
            }
            ReleaseTime::Exact(at) => line(&mut output, &format!("DTSTART:{}", stamp(at))),
        }
        if saved.cancelled {
            line(&mut output, "STATUS:CANCELLED");
        }
        line(&mut output, "END:VEVENT");
    }
    ledger.start = Some(start);
    ledger.end = Some(end);
    // Bounded history: preserve the current range plus one year around it.
    ledger
        .entries
        .retain(|_, e| (e.event.time.date() - start).num_days().abs() <= 730);
    line(&mut output, "END:VCALENDAR");
    output
}

impl Bridge {
    #[cfg(target_os = "android")]
    pub(super) fn poll_release_notification_tap(&self) {
        let pending = PENDING_TAP.lock().unwrap().clone();
        let Some((profile, key, requested)) = pending else {
            return;
        };
        let owner = profiles::ProfileStore::for_profile(&profile);
        if owner.retired().unwrap_or(true) {
            cancel_pending_notification_tap();
            return;
        }
        if !requested {
            *PENDING_TAP.lock().unwrap() = Some((profile.clone(), key.clone(), true));
            if profiles::current_id() != profile {
                self.switch_profile(&profile);
            }
        }
        let Some(app) = self.app() else {
            return;
        };
        if app.get_profiles_require_selection() || app.global::<crate::MoreBridge>().get_pin_open()
        {
            return;
        }
        if profiles::current_id() != profile {
            cancel_pending_notification_tap();
            return;
        }
        if let Ok((all, _)) = events(&owner, true, "", None)
            && all.iter().any(|event| event.key == key && !event.watched)
        {
            self.calendar_release_picked(key.into());
        }
        cancel_pending_notification_tap();
    }
    pub(super) fn setup_release_reminders(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let b = self.clone();
        app.global::<crate::CalendarBridge>()
            .on_toggle_detail(move || b.toggle_release_subscription());
        let b = self.clone();
        app.global::<crate::CalendarBridge>()
            .on_reminder_action(move |action| b.release_reminder_action(action));
        let b = self.clone();
        app.global::<crate::CalendarBridge>()
            .on_toggle_subscription(move |key| b.toggle_subscription_key(&key));
        let b = self.clone();
        app.global::<crate::CalendarBridge>()
            .on_select_export_title(move |key| {
                let mut state = b.shared.lock().unwrap();
                if !state.release_reminders.selected.remove(key.as_str()) {
                    state.release_reminders.selected.insert(key.to_string());
                }
                state.release_reminders.pending = None;
                drop(state);
                b.refresh_release_reminders();
            });
        let b = self.clone();
        app.global::<crate::CalendarBridge>()
            .on_reminder_search_changed(move |query| {
                if let Some(app) = b.app() {
                    app.global::<crate::CalendarBridge>()
                        .set_reminder_search(query);
                }
                b.refresh_release_reminders();
            });
        self.refresh_release_reminders();
        self.reconcile_release_reminders();
    }
    pub(super) fn refresh_release_reminders(&self) {
        if profile_policy::entry_blocked() {
            return;
        }
        let Some(app) = self.app() else {
            return;
        };
        let owner = profiles::ProfileStore::capture();
        let bridge = app.global::<crate::CalendarBridge>();
        let current = self.shared.lock().unwrap().modal_item.clone();
        bridge.set_detail_subscribed(
            current
                .as_ref()
                .is_some_and(|m| subscribed(&owner, &m.type_, &m.id)),
        );
        let subs = subscriptions(&owner).unwrap_or_default();
        let state: Delivery = load(&owner, DELIVERY_KEY).unwrap_or_default();
        bridge.set_delivery_enabled(state.enabled);
        bridge.set_android_notifications(cfg!(target_os = "android"));
        if bridge.get_export_start().is_empty() {
            bridge.set_export_start(Local::now().date_naive().to_string().into());
            bridge.set_export_end(
                (Local::now().date_naive() + chrono::Duration::days(90))
                    .to_string()
                    .into(),
            );
        }
        let selected = {
            let mut shared = self.shared.lock().unwrap();
            if shared.release_reminders.owner != owner.id() {
                shared.release_reminders = State {
                    owner: owner.id().into(),
                    ..Default::default()
                };
                bridge.set_export_ready(false);
                bridge.set_export_preview("".into());
            }
            shared.release_reminders.selected.clone()
        };
        let mut titles: Vec<LibraryEntry> = load(&owner, "library").unwrap_or_default();
        titles.retain(|e| {
            e.type_ != "movie" && profile_policy::allows_title(&owner, &e.type_, &e.id)
        });
        titles.sort_by(|a, b| a.name.cmp(&b.name));
        let mut keys = Vec::<SharedString>::new();
        let mut labels = Vec::<SharedString>::new();
        let mut flags = Vec::new();
        let mut selections = Vec::new();
        for title in titles {
            let key = title_key(&title.type_, &title.id);
            flags.push(subs.contains_key(&key));
            selections.push(selected.contains(&key));
            let paused = matches!(
                title.watch_status,
                WatchStatus::OnHold | WatchStatus::Dropped
            );
            labels.push(
                format!(
                    "{}{}",
                    title.name,
                    if paused {
                        text::tr(" — paused (On Hold / Dropped)")
                    } else {
                        ""
                    }
                )
                .into(),
            );
            keys.push(key.into());
        }
        // Removed titles retain their subscription until explicit unsubscribe.
        for (key, sub) in &subs {
            if !keys
                .iter()
                .any(|existing: &SharedString| existing.as_str() == key)
                && profile_policy::allows_title(&owner, &sub.type_, &sub.title_id)
            {
                keys.push(key.clone().into());
                labels.push(
                    format!("{}{}", sub.title_id, text::tr(" — paused (not in Library)")).into(),
                );
                flags.push(true);
                selections.push(false);
            }
        }
        let query = bridge.get_reminder_search().to_lowercase();
        for index in (0..keys.len()).rev() {
            if !labels[index].to_lowercase().contains(query.trim()) {
                keys.remove(index);
                labels.remove(index);
                flags.remove(index);
                selections.remove(index);
            }
        }
        bridge.set_reminder_truncated(keys.len() > 50);
        keys.truncate(50);
        labels.truncate(50);
        flags.truncate(50);
        selections.truncate(50);
        if bridge.get_tools_focus() >= 18 {
            let old_focus = bridge.get_tools_focus();
            let old_key = bridge
                .get_reminder_keys()
                .row_data(((old_focus - 18) / 2) as usize);
            let next = old_key
                .and_then(|key| keys.iter().position(|candidate| candidate == &key))
                .map(|index| 18 + index as i32 * 2 + (old_focus - 18) % 2)
                .unwrap_or(14);
            bridge.set_tools_focus(next);
        }
        bridge.set_reminder_keys(Rc::new(VecModel::from(keys)).into());
        bridge.set_reminder_labels(Rc::new(VecModel::from(labels)).into());
        bridge.set_reminder_subscribed(Rc::new(VecModel::from(flags)).into());
        bridge.set_export_selected(Rc::new(VecModel::from(selections)).into());
        bridge.set_reminder_status(
            text::release_reminder_status(
                subs.len(),
                state.last_refreshed.map(stamp).as_deref(),
                state.refresh_error.as_deref(),
            )
            .into(),
        );
        let eligible = events(&owner, true, "", None)
            .map(|(events, _)| events)
            .unwrap_or_default();
        bridge.set_digest_text(
            state
                .visible
                .iter()
                .filter_map(|e| {
                    eligible
                        .iter()
                        .find(|current| current.key == e.key && !current.watched)
                })
                .map(|e| e.summary.as_str())
                .collect::<Vec<_>>()
                .join("\n")
                .into(),
        );
    }
    fn toggle_release_subscription(&self) {
        let owner = profiles::ProfileStore::capture();
        let Some(item) = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .clone()
            .filter(|m| m.type_ != "movie")
        else {
            return;
        };
        if !profile_policy::allows_title(&owner, &item.type_, &item.id) {
            return;
        }
        self.toggle_subscription_key(&title_key(&item.type_, &item.id));
    }
    fn toggle_subscription_key(&self, key: &str) {
        let owner = profiles::ProfileStore::capture();
        let Ok((type_, id)) = serde_json::from_str::<(String, String)>(key) else {
            return;
        };
        if !profile_policy::allows_title(&owner, &type_, &id) {
            return;
        }
        let result = (|| -> Result<(), String> {
            let mut subs = subscriptions(&owner)?;
            let key = title_key(&type_, &id);
            if subs.remove(&key).is_none() {
                subs.insert(
                    key,
                    Subscription {
                        type_,
                        title_id: id,
                        extra: BTreeMap::new(),
                    },
                );
            }
            owner
                .persist_snapshot(
                    KEY,
                    &serde_json::to_string(&subs).map_err(|e| e.to_string())?,
                    false,
                )
                .map_err(|e| e.to_string())
        })();
        if let Err(e) = result {
            self.reminder_status(&e);
        } else {
            self.refresh_release_reminders();
            self.reconcile_release_reminders();
        }
    }
    fn reminder_status(&self, value: &str) {
        if let Some(app) = self.app() {
            app.global::<crate::CalendarBridge>()
                .set_reminder_status(value.into());
        }
    }
    pub(super) fn release_reminder_action(&self, action: i32) {
        let owner = profiles::ProfileStore::capture();
        match action {
            0 => {
                let result = mutate_delivery(&owner, |state| {
                    state.enabled = !state.enabled;
                    if !state.enabled {
                        state.visible.clear();
                    }
                    #[cfg(target_os = "android")]
                    if state.enabled && !release_reminders_android::enable_notifications() {
                        return Err(text::tr(
                            "Allow notifications in Android settings, then enable delivery again",
                        )
                        .into());
                    }
                    Ok(())
                });
                if let Err(error) = result {
                    self.reminder_status(&error);
                } else {
                    self.refresh_release_reminders();
                    self.reconcile_release_reminders();
                }
            }
            1 | 2 | 3 | 6 => self.save_release_calendar(action),
            10..=13 => {
                let Some(app) = self.app() else {
                    return;
                };
                let ui = app.global::<crate::CalendarBridge>();
                let value = if action < 12 {
                    ui.get_export_start()
                } else {
                    ui.get_export_end()
                };
                if let Ok(day) = NaiveDate::parse_from_str(&value, "%Y-%m-%d") {
                    let value = (day
                        + chrono::Duration::days(if action % 2 == 0 { -1 } else { 1 }))
                    .to_string();
                    if action < 12 {
                        ui.set_export_start(value.into());
                    } else {
                        ui.set_export_end(value.into());
                    }
                    ui.set_export_ready(false);
                    self.shared.lock().unwrap().release_reminders.pending = None;
                }
            }
            4 => {
                if let Err(error) = mutate_delivery(&owner, |state| {
                    state
                        .dismissed
                        .extend(state.visible.iter().map(|event| event.key.clone()));
                    state.visible.clear();
                    Ok(())
                }) {
                    self.reminder_status(&error);
                } else {
                    self.refresh_release_reminders();
                    self.reconcile_release_reminders();
                }
            }
            5 => {
                let state: Delivery = load(&owner, DELIVERY_KEY).unwrap_or_default();
                if let Some(event) = state
                    .visible
                    .first()
                    .filter(|e| profile_policy::allows_title(&owner, &e.type_, &e.title_id))
                {
                    self.calendar_release_picked(event.key.clone().into());
                }
            }
            _ => {}
        }
    }
    fn release_export_token(&self, app: &AppWindow) -> String {
        let ui = app.global::<crate::CalendarBridge>();
        let mut selected = self
            .shared
            .lock()
            .unwrap()
            .release_reminders
            .selected
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        selected.sort();
        serde_json::to_string(&(
            ui.get_export_start().to_string(),
            ui.get_export_end().to_string(),
            ui.get_export_scope(),
            app.get_calendar_search().to_string(),
            selected,
        ))
        .unwrap()
    }
    fn save_release_calendar(&self, action: i32) {
        let owner = profiles::ProfileStore::capture();
        if !profile_policy::management_allowed(&owner) {
            self.reminder_status(text::tr("Unlock profile management first"));
            return;
        }
        let Some(app) = self.app() else {
            return;
        };
        if action == 6 {
            let pending = {
                let mut shared = self.shared.lock().unwrap();
                if shared.release_reminders.owner != owner.id() {
                    shared.release_reminders.pending = None;
                    return;
                }
                shared.release_reminders.pending.take()
            };
            let Some((ledger, ics, revision, inputs)) = pending else {
                return;
            };
            if revision != profile_policy::revision(&owner)
                || inputs != self.release_export_token(&app)
            {
                self.reminder_status(text::tr("Profile changed; preview this export again"));
                return;
            }
            let b = self.clone();
            let session = profiles::capture();
            documents::save(
                "nova-releases.ics",
                "text/calendar",
                ics.into_bytes(),
                move |result| {
                    if matches!(result, Ok(true))
                        && let Err(error) =
                            owner.write(EXPORT_KEY, &serde_json::to_string(&ledger).unwrap())
                    {
                        storage::report(error);
                    }
                    let _ = profiles::invoke_from_event_loop(move || {
                        if profiles::is_current(&session)
                            && let Err(error) = result
                        {
                            b.reminder_status(&error);
                        }
                    });
                },
            );
            app.global::<crate::CalendarBridge>()
                .set_export_ready(false);
            return;
        }
        let bridge = app.global::<crate::CalendarBridge>();
        let result = (|| -> Result<_, String> {
            let mut ledger: ExportLedger = load(&owner, EXPORT_KEY)?;
            if action != 3 || ledger.namespace.is_empty() {
                ledger = ExportLedger {
                    namespace: hex_digest(format!(
                        "{}:{}:{}",
                        owner.id(),
                        nova_config::now_ms(),
                        nova_sync::local_device().unwrap_or(0)
                    )),
                    only_subscribed: bridge.get_export_scope() == 0,
                    query: if bridge.get_export_scope() == 2 {
                        app.get_calendar_search().to_string()
                    } else {
                        String::new()
                    },
                    selected: (bridge.get_export_scope() == 3).then(|| {
                        self.shared
                            .lock()
                            .unwrap()
                            .release_reminders
                            .selected
                            .clone()
                    }),
                    ..Default::default()
                };
            }
            let start = NaiveDate::parse_from_str(bridge.get_export_start().as_str(), "%Y-%m-%d")
                .map_err(|_| text::tr("Enter valid start and end dates").to_string())?;
            let end = NaiveDate::parse_from_str(bridge.get_export_end().as_str(), "%Y-%m-%d")
                .map_err(|_| text::tr("Enter valid start and end dates").to_string())?;
            if end < start || (end - start).num_days() > 730 {
                return Err(text::tr("Choose an ordered date range of at most two years").into());
            }
            let (events, omitted) = events(
                &owner,
                ledger.only_subscribed,
                &ledger.query,
                ledger.selected.as_ref(),
            )?;
            let hidden = ledger
                .entries
                .iter()
                .filter(|(_, entry)| {
                    !profile_policy::allows_title(&owner, &entry.event.type_, &entry.event.title_id)
                })
                .map(|(key, entry)| (key.clone(), entry.clone()))
                .collect::<Vec<_>>();
            for (key, _) in &hidden {
                ledger.entries.remove(key);
            }
            let ics = export_ics(&mut ledger, &events, start, end, Utc::now().timestamp());
            ledger.entries.extend(hidden);
            Ok((ledger, ics, omitted))
        })();
        let (ledger, ics, omitted) = match result {
            Ok(v) => v,
            Err(e) => {
                self.reminder_status(&e);
                return;
            }
        };
        let emitted: HashSet<_> = ics
            .split("\r\n")
            .filter_map(|line| line.strip_prefix("UID:"))
            .collect();
        let count = ledger
            .entries
            .values()
            .filter(|event| !event.cancelled && emitted.contains(event.uid.as_str()))
            .count();
        let series = ledger
            .entries
            .values()
            .filter(|e| !e.cancelled && emitted.contains(e.uid.as_str()))
            .map(|e| title_key(&e.event.type_, &e.event.title_id))
            .collect::<HashSet<_>>()
            .len();
        let cancelled = ledger
            .entries
            .values()
            .filter(|e| e.cancelled && emitted.contains(e.uid.as_str()))
            .count();
        bridge.set_export_preview(
            text::release_export_preview(series, count, omitted, cancelled).into(),
        );
        bridge.set_export_ready(true);
        let inputs = self.release_export_token(&app);
        self.shared.lock().unwrap().release_reminders.pending =
            Some((ledger, ics, profile_policy::revision(&owner), inputs));
    }
    pub(super) fn reconcile_release_reminders(&self) {
        self.refresh_release_reminders();
        if DELIVERY_RUNNING.swap(true, Ordering::AcqRel) {
            return;
        }
        // Profile activation never owns background delivery. The captured
        // durable owners can finish after selection changes, without UI work.
        let bridge = self.clone();
        let session = profiles::capture();
        std::thread::spawn(move || {
            struct Running;
            impl Drop for Running {
                fn drop(&mut self) {
                    DELIVERY_RUNNING.store(false, Ordering::Release);
                }
            }
            let _running = Running;
            if let Ok(profiles) = profiles::list() {
                for profile in profiles {
                    let owner = profiles::ProfileStore::for_profile(&profile.id);
                    if let Err(error) = reconcile_owner(&owner) {
                        eprintln!("nova release reminders: {error}");
                    }
                }
            }
            let _ = profiles::invoke_from_event_loop(move || {
                if profiles::is_current(&session) {
                    bridge.refresh_release_reminders();
                }
            });
        });
    }
}

pub(super) fn reconcile_owner(owner: &profiles::ProfileStore) -> Result<(), String> {
    let _serial = DELIVERY_SERIAL.lock().unwrap();
    if owner.retired().map_err(|e| e.to_string())? {
        return Ok(());
    }
    let mut state: Delivery = load(owner, DELIVERY_KEY)?;
    if !state.enabled {
        #[cfg(target_os = "android")]
        release_reminders_android::notify(owner.id(), "", "", false, "", "[]");
        return Ok(());
    }
    #[cfg(target_os = "android")]
    if state.enabled && !release_reminders_android::delivery_available() {
        return Ok(());
    }

    let inputs = [profile_policy::KEY, KEY, "library", EPISODE_PROGRESS_KEY]
        .into_iter()
        .map(|key| owner.read(key).map(|raw| (key, raw)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let (all, _) = events(owner, true, "", None)?;
    let now = Local::now();
    let before = serde_json::to_string(&state).unwrap();
    let _alert = reconcile_delivery(
        &mut state,
        &all,
        now.timestamp(),
        now.date_naive(),
        now.num_seconds_from_midnight(),
    );
    let raw = serde_json::to_string(&state).unwrap();
    if inputs
        .iter()
        .any(|(key, before)| owner.read(key).as_ref().ok() != Some(before))
    {
        return Ok(());
    }
    // Commit the receipt before touching the OS: a crash can suppress a single
    // alert but cannot produce repeated alerts on every job/restart.
    if raw != before {
        owner.write(DELIVERY_KEY, &raw).map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "android")]
    {
        let title = profiles::list()
            .ok()
            .and_then(|profiles| {
                profiles
                    .into_iter()
                    .find(|profile| profile.id == owner.id())
            })
            .map(|profile| profile.name)
            .unwrap_or_else(|| "Nova".into());
        let native = native_events(&state);
        let keys =
            serde_json::to_string(&native.iter().map(|event| &event.key).collect::<Vec<_>>())
                .unwrap();
        let body = if native.is_empty() {
            String::new()
        } else {
            format!(
                "{}\n{}",
                text::tr("Released today"),
                native
                    .iter()
                    .map(|event| event.summary.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        release_reminders_android::notify(
            owner.id(),
            &title,
            &body,
            _alert,
            native.first().map(|event| event.key.as_str()).unwrap_or(""),
            &keys,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(id: &str, raw: &str) -> Event {
        Event {
            key: format!("series:show:{id}"),
            type_: "series".into(),
            title_id: "show".into(),
            episode_id: id.into(),
            summary: "Čarobni, show;\\\nnew".into(),
            description: "long č".repeat(30),
            time: ReleaseTime::parse(raw).unwrap(),
            watched: false,
            aliases: Vec::new(),
        }
    }
    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 23).unwrap()
    }
    #[test]
    fn digest_restart_late_refresh_date_changes_and_travel_never_repeat() {
        let a = event("a", "2026-10-23");
        let b = event("b", "2026-10-23");
        let mut state = Delivery {
            enabled: true,
            ..Default::default()
        };
        assert!(!reconcile_delivery(
            &mut state,
            &[a.clone()],
            1,
            day(),
            8 * 3600
        ));
        assert!(reconcile_delivery(
            &mut state,
            &[a.clone()],
            2,
            day(),
            9 * 3600
        ));
        let mut state: Delivery =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert!(!reconcile_delivery(
            &mut state,
            &[a.clone()],
            3,
            day(),
            10 * 3600
        ));
        assert!(!reconcile_delivery(
            &mut state,
            &[a.clone(), b],
            4,
            day(),
            10 * 3600
        ));
        let mut changed = a;
        changed.time = ReleaseTime::Day(day().succ_opt().unwrap());
        assert!(!reconcile_delivery(
            &mut state,
            &[changed],
            5,
            day().succ_opt().unwrap(),
            9 * 3600
        ));
        assert_eq!(state.delivered.len(), 2);
    }
    #[test]
    fn ics_identity_folding_dates_revisions_cancellations_and_ranges() {
        let a = event("a", "2026-10-23");
        let b = event("b", "2026-10-23T20:00:00+02:00");
        let mut ledger = ExportLedger {
            namespace: "portable".into(),
            ..Default::default()
        };
        let first = export_ics(
            &mut ledger,
            &[a.clone(), b.clone()],
            day(),
            day() + chrono::Duration::days(90),
            1,
        );
        assert!(first.contains("DTSTART:20261023T180000Z\r\n"));
        assert!(first.contains("DTEND;VALUE=DATE:20261024\r\n"));
        assert!(first.contains("SUMMARY:Čarobni\\, show\\;\\\\\\nnew"));
        assert!(first.split("\r\n").all(|line| line.len() <= 75));
        let saved = ledger.entries[&a.key].clone();
        let _ = export_ics(
            &mut ledger,
            &[a.clone(), b],
            day(),
            day() + chrono::Duration::days(90),
            2,
        );
        assert_eq!(ledger.entries[&a.key].modified, 1);
        assert_eq!(ledger.entries[&a.key].uid, saved.uid);
        let mut changed = a.clone();
        changed.summary.push('!');
        let _ = export_ics(&mut ledger, &[changed.clone()], day(), day(), 3);
        assert_eq!(ledger.entries[&a.key].sequence, 1);
        let cancelled = export_ics(&mut ledger, &[], day(), day(), 4);
        assert!(cancelled.contains("STATUS:CANCELLED"));
        let _ = export_ics(&mut ledger, &[], day(), day(), 5);
        assert_eq!(ledger.entries[&a.key].sequence, 2);
        let mut restored: ExportLedger =
            serde_json::from_str(&serde_json::to_string(&ledger).unwrap()).unwrap();
        let _ = export_ics(&mut restored, &[changed], day(), day(), 6);
        assert_eq!(restored.entries[&a.key].uid, saved.uid);
    }
    #[test]
    fn native_dismissal_survives_restart_and_late_updates_do_not_alert_again() {
        let a = event("a", "2026-10-23");
        let b = event("b", "2026-10-23");
        let mut state = Delivery {
            enabled: true,
            ..Default::default()
        };
        assert!(reconcile_delivery(
            &mut state,
            std::slice::from_ref(&a),
            100,
            day(),
            9 * 3600
        ));
        dismiss_native(&mut state, std::slice::from_ref(&a.key));
        let mut state: Delivery =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert!(native_events(&state).is_empty());
        assert!(!reconcile_delivery(
            &mut state,
            &[a, b.clone()],
            101,
            day(),
            10 * 3600
        ));
        assert_eq!(native_events(&state).len(), 1);
        assert_eq!(native_events(&state)[0].key, b.key);
        dismiss_native(&mut state, std::slice::from_ref(&b.key));
        assert!(native_events(&state).is_empty());
    }
    #[test]
    fn overdue_digest_stops_at_next_nine_and_overlapping_civil_times_alert_once() {
        let release = event("late", "2026-10-23");
        let mut state = Delivery {
            enabled: true,
            ..Default::default()
        };
        assert!(reconcile_delivery(
            &mut state,
            std::slice::from_ref(&release),
            100,
            day().succ_opt().unwrap(),
            8 * 3600
        ));
        assert!(!reconcile_delivery(
            &mut state,
            std::slice::from_ref(&release),
            101,
            day().succ_opt().unwrap(),
            8 * 3600
        ));
        let mut expired = Delivery {
            enabled: true,
            ..Default::default()
        };
        assert!(!reconcile_delivery(
            &mut expired,
            std::slice::from_ref(&release),
            102,
            day().succ_opt().unwrap(),
            9 * 3600
        ));
        let mut gap = Delivery {
            enabled: true,
            ..Default::default()
        };
        assert!(reconcile_delivery(
            &mut gap,
            std::slice::from_ref(&release),
            103,
            day(),
            10 * 3600
        ));
        assert!(!reconcile_delivery(
            &mut gap,
            std::slice::from_ref(&release),
            104,
            day(),
            9 * 3600
        ));
        assert_eq!(gap.delivered.len(), 1);
    }
    #[test]
    fn export_window_shifts_do_not_cancel_and_confirmed_alias_keeps_uid() {
        let mut old = event("old", "2026-10-23");
        old.aliases = vec!["[\"series\",\"tt7:1:1\"]".into()];
        let mut ledger = ExportLedger {
            namespace: "portable".into(),
            ..Default::default()
        };
        let _ = export_ics(&mut ledger, std::slice::from_ref(&old), day(), day(), 1);
        let uid = ledger.entries[&old.key].uid.clone();
        let shifted = export_ics(
            &mut ledger,
            std::slice::from_ref(&old),
            day().succ_opt().unwrap(),
            day().succ_opt().unwrap(),
            2,
        );
        assert!(!shifted.contains("STATUS:CANCELLED"));
        assert_eq!(ledger.entries[&old.key].sequence, 0);
        let mut alias = old.clone();
        alias.key = "series:new:episode".into();
        alias.title_id = "new".into();
        alias.episode_id = "episode".into();
        let same = export_ics(&mut ledger, std::slice::from_ref(&alias), day(), day(), 3);
        assert_eq!(same.matches("BEGIN:VEVENT").count(), 1);
        assert_eq!(ledger.entries[&old.key].uid, uid);
        assert_eq!(ledger.entries[&old.key].sequence, 0);
        alias.time = ReleaseTime::Day(day() + chrono::Duration::days(100));
        let moved = export_ics(&mut ledger, &[alias], day(), day(), 4);
        assert!(moved.contains("DTSTART;VALUE=DATE:20270131"));
        assert_eq!(ledger.entries[&old.key].sequence, 1);
    }
    #[cfg(feature = "desktop")]
    fn blocked_action(owner: &profiles::ProfileStore, label: &'static str, action: impl FnOnce()) {
        let (loaded_tx, loaded_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker_owner = owner.clone();
        let worker = std::thread::spawn(move || {
            let _guard = delivery_guard();
            let mut state: Delivery = load(&worker_owner, DELIVERY_KEY).unwrap();
            loaded_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            state.delivered.insert(label.into(), 456);
            worker_owner
                .write(DELIVERY_KEY, &serde_json::to_string(&state).unwrap())
                .unwrap();
        });
        loaded_rx.recv().unwrap();
        let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
        BEFORE_DELIVERY_LOCK.with(|hook| *hook.borrow_mut() = Some(attempt_tx));
        let controller = std::thread::spawn(move || {
            let entered = attempt_rx.recv_timeout(Duration::from_secs(2)).is_ok();
            release_tx.send(()).unwrap();
            entered
        });
        action();
        assert!(
            controller.join().unwrap(),
            "production mutation did not acquire delivery guard: {label}"
        );
        worker.join().unwrap();
        assert_eq!(
            load::<Delivery>(owner, DELIVERY_KEY)
                .unwrap()
                .delivered
                .get(label),
            Some(&456)
        );
    }
    #[test]
    fn profile_subscription_sync_and_delivery_owners_are_isolated_and_retired() {
        const ROOT: &str = "NOVA_RELEASE_REMINDERS_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-reminders-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output=std::process::Command::new(std::env::current_exe().unwrap()).env(ROOT,&root)
                .args(["--exact","app::release_reminders::tests::profile_subscription_sync_and_delivery_owners_are_isolated_and_retired","--nocapture"]).output().unwrap();
            let _ = std::fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        nova_storage::init_at(&std::path::PathBuf::from(root));
        profiles::initialize().unwrap();
        let alice = profiles::ProfileStore::capture();
        let bob = profiles::ProfileStore::for_profile(&profiles::create("Bob").unwrap());
        let key = title_key("series", "same");
        let raw = serde_json::json!({key.clone():{"type_":"series","title_id":"same","future":7}})
            .to_string();
        alice.persist_snapshot(KEY, &raw, false).unwrap();
        assert!(subscribed(&alice, "series", "same"));
        assert!(!subscribed(&alice, "movie", "same"));
        assert!(!subscribed(&bob, "series", "same"));
        let store = nova_sync::local_store().unwrap();
        assert!(
            store
                .lock()
                .unwrap()
                .record(&alice.domain(DOMAIN), &key)
                .is_some()
        );
        alice
            .write(
                DELIVERY_KEY,
                &serde_json::to_string(&Delivery {
                    enabled: true,
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        assert!(!load::<Delivery>(&bob, DELIVERY_KEY).unwrap().enabled);
        // Hold the worker after its old enabled=true snapshot is loaded.
        // Both user mutations must wait, then merge the committed receipt.
        let (loaded_tx, loaded_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker_owner = alice.clone();
        let worker = std::thread::spawn(move || {
            let _serial = delivery_guard();
            let mut state: Delivery = load(&worker_owner, DELIVERY_KEY).unwrap();
            loaded_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            state.delivered.insert("raced-receipt".into(), 123);
            worker_owner
                .write(DELIVERY_KEY, &serde_json::to_string(&state).unwrap())
                .unwrap();
        });
        loaded_rx.recv().unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let disable_owner = alice.clone();
        let start = started_tx.clone();
        let done = done_tx.clone();
        let disable = std::thread::spawn(move || {
            start.send(()).unwrap();
            mutate_delivery(&disable_owner, |state| {
                state.enabled = false;
                Ok(())
            })
            .unwrap();
            done.send(()).unwrap();
        });
        let dismiss_owner = alice.clone();
        let dismiss = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            mutate_delivery(&dismiss_owner, |state| {
                state.visible.clear();
                Ok(())
            })
            .unwrap();
            done_tx.send(()).unwrap();
        });
        started_rx.recv().unwrap();
        started_rx.recv().unwrap();
        assert!(done_rx.try_recv().is_err());
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        disable.join().unwrap();
        dismiss.join().unwrap();
        let final_state: Delivery = load(&alice, DELIVERY_KEY).unwrap();
        assert!(!final_state.enabled);
        assert!(final_state.visible.is_empty());
        assert_eq!(final_state.delivered.get("raced-receipt"), Some(&123));
        assert!(
            serde_json::to_string(&subscriptions(&alice).unwrap())
                .unwrap()
                .contains("future")
        );
        let record = records(&raw).unwrap();
        assert_eq!(record.len(), 1);
        #[cfg(feature = "desktop")]
        {
            i_slint_backend_testing::init_integration_test_with_system_time();
            let app = AppWindow::new().unwrap();
            let player = crate::player::Player::setup(&app);
            let (hi, _) = mpsc::channel();
            let (lo, _) = mpsc::channel();
            let bridge = Bridge::new(
                app.as_weak(),
                PosterTx { hi, lo },
                Arc::new(AtomicU64::new(0)),
                player,
                DownloadCoordinator::new(
                    nova_config::app_cache_dir().join("reminder-test-downloads"),
                ),
            );
            mutate_delivery(&alice, |state| {
                state.enabled = true;
                Ok(())
            })
            .unwrap();
            blocked_action(&alice, "ui-disable", || bridge.release_reminder_action(0));
            assert!(!load::<Delivery>(&alice, DELIVERY_KEY).unwrap().enabled);
            blocked_action(&alice, "ui-dismiss", || bridge.release_reminder_action(4));
            blocked_action(&alice, "ui-unsubscribe", || {
                bridge.toggle_subscription_key(&key)
            });
            assert!(!subscribed(&alice, "series", "same"));
            blocked_action(&alice, "policy-persist", || {
                profile_policy::save(
                    &alice,
                    &profile_policy::ProfilePolicy {
                        pin: "0012".into(),
                        require_pin_on_entry: true,
                        ..Default::default()
                    },
                )
                .unwrap()
            });
            assert_eq!(profile_policy::load(&alice).unwrap().pin, "0012");
        }
        profiles::delete(bob.id()).unwrap();
        assert!(bob.write(DELIVERY_KEY, "{}").is_err());
    }
    #[test]
    fn invalid_dates_and_ambiguous_times_do_not_invent_exact_time() {
        assert!(ReleaseTime::parse("2026-02-30").is_none());
        assert!(ReleaseTime::parse("2026-10-23T99:99:99+02:00").is_none());
        assert!(matches!(
            ReleaseTime::parse("2026-10-23T09:00:00"),
            Some(ReleaseTime::Day(_))
        ));
        assert_ne!(
            uid("ns", "[\"movie\",\"1\",\"2\"]"),
            uid("ns", "[\"series\",\"1\",\"2\"]")
        );
    }
}

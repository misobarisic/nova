//! Cross-device sync integration.
//!
//! Bridges the app's data model onto `nova-sync`'s generic record store:
//!
//! * **Decompose** — each persisted collection is split into per-record keys
//!   (`library/{id}`, `progress/{key}`, `addons/{url}`, `settings/{field}`,
//!   `category/{name}`) and pushed with [`notify_*`] whenever the app
//!   persists. Removed keys become tombstones (detected by diffing against
//!   the store).
//! * **Materialize** — remote changes announced by the engine are rebuilt
//!   into the app's own types here and re-rendered through the existing
//!   apply/refresh paths.
//!
//! Applying remote records sets [`APPLYING`] so the notify hooks stay quiet
//! (a remote change must not echo back as a local one).

use super::*;
use std::collections::BTreeSet;

pub(crate) const DOMAIN_LIBRARY: &str = "library";
pub(crate) const DOMAIN_PROGRESS: &str = "progress";
pub(crate) const DOMAIN_LAST_STREAM: &str = last_stream::DOMAIN;
pub(crate) const DOMAIN_PLAYBACK_PREFERENCES: &str = "playback_preferences";
use super::playback_preferences::PREFERENCES_KEY;
use nova_config::playback_preferences::PreferenceValue;
pub(crate) const DOMAIN_VIEWING_ACTIVITY: &str = "viewing_activity";
pub(crate) const DOMAIN_HISTORY_HIDDEN: &str = "watch_history_hidden";
pub(crate) const DOMAIN_NEW_EPISODES_HIDDEN: &str = "new_episodes_hidden";
pub(crate) const DOMAIN_PACKAGES: &str = addon_packages::DOMAIN;
pub(crate) const DOMAIN_PLUGIN_SETTINGS: &str = "plugin_settings";
pub(crate) const DOMAIN_ADDONS: &str = "addons";
pub(crate) const DOMAIN_SETTINGS: &str = "settings";
pub(crate) const DOMAIN_CATEGORY: &str = "category";
/// Continue Watching removals, one record per item id (`hidden_at` unix secs).
/// App-side only: a new domain name travels as a string, so peers that predate
/// it simply ignore it.
pub(crate) const DOMAIN_CONTINUE_HIDDEN: &str = "continue_hidden";

/// Account membership is shared; every user-data domain is qualified even for
/// the migrated default profile. Switching locally never changes this directory.
pub(super) const SCOPED_DOMAINS: &[&str] = &[
    DOMAIN_LIBRARY,
    DOMAIN_PROGRESS,
    DOMAIN_LAST_STREAM,
    DOMAIN_PLAYBACK_PREFERENCES,
    DOMAIN_VIEWING_ACTIVITY,
    DOMAIN_HISTORY_HIDDEN,
    DOMAIN_NEW_EPISODES_HIDDEN,
    DOMAIN_PACKAGES,
    DOMAIN_PLUGIN_SETTINGS,
    DOMAIN_ADDONS,
    DOMAIN_SETTINGS,
    DOMAIN_CATEGORY,
    DOMAIN_CONTINUE_HIDDEN,
    nova_tracking::peer::TRACKING_DOMAIN,
    nova_tracking::TRACKING_COMPLETION_DOMAIN,
    stremio::DOMAIN,
    list_services::DOMAIN,
];

pub(super) fn profile_domain(domain: &str) -> String {
    if SCOPED_DOMAINS.contains(&domain) {
        profiles::ProfileStore::capture().domain(domain)
    } else {
        domain.to_owned()
    }
}

/// Preserve legacy record clocks and snapshots in one transaction. In particular
/// progress action registers must not be restamped as a fresh watch action.
pub(super) fn migrate_legacy_profiles() -> nova_sync::Result<()> {
    let snapshots = profiles::legacy_snapshot_migration_entries()?;
    let owner = nova_sync::local_store()?;
    let mut store = owner.lock().unwrap();
    for (key, value) in snapshots {
        store.queue_extra(&key, value);
    }
    for domain in SCOPED_DOMAINS {
        store.migrate_domain(domain, &nova_sync::profile_domain("default", domain))?;
    }
    store.save()
}

// Origin is scoped to synchronous work on this thread, not a global flag that
// could suppress an unrelated mutation on another worker. Nesting is safe.
thread_local! { static APPLYING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }

/// Whether the current write is an apply of remote data (suppresses echo).
pub(crate) fn applying() -> bool {
    APPLYING.with(|depth| depth.get() != 0)
}

pub(super) struct ApplyingGuard(std::marker::PhantomData<Rc<()>>);

impl ApplyingGuard {
    pub(super) fn new() -> Self {
        APPLYING.with(|depth| depth.set(depth.get() + 1));
        Self(std::marker::PhantomData)
    }
}

impl Drop for ApplyingGuard {
    fn drop(&mut self) {
        APPLYING.with(|depth| depth.set(depth.get() - 1));
    }
}

struct ProjectionStore(std::sync::Arc<Mutex<nova_sync::Store>>);
impl ProjectionStore {
    fn records(&self, domain: &str) -> Vec<(String, String)> {
        self.0.lock().unwrap().records(&profile_domain(domain))
    }
}
fn projection_store() -> Option<ProjectionStore> {
    nova_sync::local_store().ok().map(ProjectionStore)
}

/// Wire format for one synced addon entry. `label` travels so every device
/// shows the same name (empty from older peers, then derived locally).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SyncedAddon {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    capabilities: Option<nova_providers::ProviderCapabilities>,
    #[serde(default)]
    label: String,
    enabled: bool,
    #[serde(default)]
    configure_ok: Option<bool>,
}

fn merged_addon_capabilities(
    incoming: Option<nova_providers::ProviderCapabilities>,
    local: Option<nova_providers::ProviderCapabilities>,
) -> nova_providers::ProviderCapabilities {
    incoming.or(local).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Decompose: push local writes into the sync store
// ---------------------------------------------------------------------------

/// Insert/update `records` and tombstone any live key of `domain` not
/// present in the list, as one batched store write (a seed of a large library
/// would otherwise save once per record).
fn sync_records(domain: &str, records: Vec<(String, String, u64)>) {
    if !profiles::writable(&profiles::current_id()) {
        return;
    }
    let key = match domain {
        DOMAIN_LIBRARY => "library",
        DOMAIN_PROGRESS => "episode_progress",
        DOMAIN_PLAYBACK_PREFERENCES => PREFERENCES_KEY,
        DOMAIN_VIEWING_ACTIVITY => VIEWING_ACTIVITY_KEY,
        DOMAIN_NEW_EPISODES_HIDDEN => NEW_EPISODES_HIDDEN_KEY,
        DOMAIN_HISTORY_HIDDEN => watch_history::HIDDEN_KEY,
        DOMAIN_ADDONS => "addons",
        DOMAIN_PLUGIN_SETTINGS => plugin_settings::KEY,
        DOMAIN_PACKAGES => addon_packages::KEY,
        DOMAIN_SETTINGS | DOMAIN_CATEGORY => "settings",
        DOMAIN_CONTINUE_HIDDEN => "continue_hidden",
        _ => "",
    };
    if !writable_key(key) {
        return;
    }
    if let Err(e) =
        nova_sync::commit_snapshot(&profile_domain(domain), &records, None, applying(), false)
    {
        storage::report(storage::Error::new(storage::ErrorKind::Transaction, e));
    }
}

/// App snapshots and record mutations share one transaction, even with sync
/// disabled. A persisted baseline is the only deletion authority.
type SnapshotDomainRecords = (&'static str, Vec<(String, String, u64)>);

pub(crate) fn persist_sync_snapshot(key: &str, raw: &str, seed: bool) -> nova_sync::Result<()> {
    if profiles::scoped_key(key) && !profiles::writable(&profiles::current_id()) {
        return Err(
            storage::Error::new(storage::ErrorKind::Schema, "profile has been deleted").into(),
        );
    }
    persistence::register_collections();
    persistence::flush()
        .map_err(|error| storage::Error::new(storage::ErrorKind::Transaction, error))?;
    if !writable_key(key) {
        return Err(storage::Error::new(
            storage::ErrorKind::Schema,
            "unreadable snapshot retained",
        )
        .into());
    }
    let domains: Vec<SnapshotDomainRecords> = match key {
        plugin_settings::KEY => vec![(DOMAIN_PLUGIN_SETTINGS, plugin_settings::records(raw)?)],
        addon_packages::KEY => vec![(
            DOMAIN_PACKAGES,
            addon_packages::records(raw)
                .map_err(|e| storage::Error::new(storage::ErrorKind::Schema, e))?,
        )],
        "library" => {
            let entries: Vec<LibraryEntry> = serde_json::from_str(raw)?;
            vec![(
                DOMAIN_LIBRARY,
                entries
                    .iter()
                    .map(|e| Ok((e.id.clone(), serde_json::to_string(e)?, 0)))
                    .collect::<Result<_, serde_json::Error>>()?,
            )]
        }
        PREFERENCES_KEY => {
            let map: HashMap<String, PreferenceValue> = serde_json::from_str(raw)?;
            vec![(
                DOMAIN_PLAYBACK_PREFERENCES,
                map.into_iter()
                    .map(|(key, value)| Ok((key, serde_json::to_string(&value)?, 0)))
                    .collect::<Result<_, serde_json::Error>>()?,
            )]
        }
        "episode_progress" => {
            let map: HashMap<String, EpisodeProgress> = serde_json::from_str(raw)?;
            vec![(
                DOMAIN_PROGRESS,
                map.iter()
                    .map(|(k, v)| Ok((k.clone(), serde_json::to_string(v)?, v.updated_at_secs)))
                    .collect::<Result<_, serde_json::Error>>()?,
            )]
        }
        VIEWING_ACTIVITY_KEY => {
            let activity: HashMap<String, ViewingActivity> = serde_json::from_str(raw)?;
            vec![(DOMAIN_VIEWING_ACTIVITY, activity_records(&activity)?)]
        }
        NEW_EPISODES_HIDDEN_KEY => {
            let hidden: HashMap<String, Vec<String>> = serde_json::from_str(raw)?;
            vec![(
                DOMAIN_NEW_EPISODES_HIDDEN,
                hidden
                    .iter()
                    .map(|(id, ids)| Ok((id.clone(), serde_json::to_string(ids)?, 0)))
                    .collect::<Result<_, serde_json::Error>>()?,
            )]
        }
        "continue_hidden" | watch_history::HIDDEN_KEY => {
            let map: HashMap<String, u64> = serde_json::from_str(raw)?;
            vec![(
                if key == watch_history::HIDDEN_KEY {
                    DOMAIN_HISTORY_HIDDEN
                } else {
                    DOMAIN_CONTINUE_HIDDEN
                },
                map.into_iter()
                    .map(|(k, v)| (k, v.to_string(), v))
                    .collect(),
            )]
        }
        "addons" => {
            let addons: Vec<AddonStore> = serde_json::from_str(raw)?;
            vec![(DOMAIN_ADDONS, addon_records(&addons)?)]
        }
        "settings" => {
            let settings: CacheSettings = serde_json::from_str(raw)?;
            vec![
                (DOMAIN_SETTINGS, live_settings_fields(&settings)),
                (
                    DOMAIN_CATEGORY,
                    settings
                        .categories
                        .iter()
                        .map(|n| (n.clone(), "1".to_string(), 0))
                        .collect(),
                ),
            ]
        }
        _ => {
            if metadata_cache_key(key) {
                storage::try_set_cached_str(key, raw)?;
            } else {
                storage::try_set_str(key, raw)?;
            }
            return Ok(());
        }
    };
    // Playback and its activity checkpoint share a transaction. Availability
    // refreshes never produce an Active record, and remote apply never echoes.
    let projection = applying() && !stremio_events::external();
    let activity_update = if key == EPISODE_PROGRESS_KEY && !projection && !seed {
        let mut activity = read_viewing_activity();
        if writable_key(VIEWING_ACTIVITY_KEY) {
            let before = activity.clone();
            let progress: HashMap<String, EpisodeProgress> = serde_json::from_str(raw)?;
            let old = read_progress_map();
            // Progress saves are frequent. Read episode metadata only for
            // affected titles, rather than decoding the entire saved library.
            let affected: HashSet<_> = progress
                .iter()
                .filter(|(key, value)| {
                    old.get(*key).is_none_or(|previous| {
                        previous.watched != value.watched
                            || previous.position_secs != value.position_secs
                            || previous.unwatched_at_secs != value.unwatched_at_secs
                    })
                })
                .map(|(_, value)| value.series_id.as_str())
                .collect();
            let entries: Vec<_> = read_persisted_library()
                .into_iter()
                .filter(|entry| affected.contains(entry.id.as_str()))
                .collect();
            reconcile_viewing_activity(
                &mut activity,
                &entries,
                &old,
                &progress,
                |e| read_episodes_cache_for(&e.type_, &e.id).unwrap_or_default(),
                today_days(),
            );
            (activity != before).then_some(activity)
        } else {
            None
        }
    } else {
        None
    };
    let owner = nova_sync::local_store()?;
    let dev = nova_sync::local_device()?;
    let mut store = owner.lock().unwrap();
    if profiles::scoped_key(key)
        && store
            .record("profile_deletions", &profiles::current_id())
            .is_some_and(|r| !r.is_deleted())
    {
        return Err(
            storage::Error::new(storage::ErrorKind::Schema, "profile has been deleted").into(),
        );
    }
    let mut changed = store.has_dirty_records();
    if let Some(activity) = activity_update {
        changed |= nova_sync::prepare_snapshot(
            &mut store,
            dev,
            &profile_domain(DOMAIN_VIEWING_ACTIVITY),
            &activity_records(&activity)?,
            None,
            false,
            false,
        )?;
        store.queue_extra(
            &profiles::storage_key(VIEWING_ACTIVITY_KEY),
            Some(serde_json::to_string(&activity)?),
        );
    }
    for (domain, records) in domains {
        // Untouched installation defaults are projection-only. Legacy installs
        // with an existing settings object conservatively retain all choices.
        let untouched =
            seed && domain == DOMAIN_SETTINGS && storage::try_get_str("settings")?.is_none();
        changed |= nova_sync::prepare_snapshot(
            &mut store,
            dev,
            &profile_domain(domain),
            &records,
            None,
            projection || untouched,
            seed,
        )?;
    }
    let snapshot_key = profiles::storage_key(key);
    let old = store.extra_value(&snapshot_key)?;
    stremio_events::capture(&mut store, key, old.as_deref(), raw, seed)?;
    changed |= store.has_dirty_records();
    if key == EPISODE_PROGRESS_KEY {
        tracking::journal_progress(&mut store, old.as_deref(), raw, applying(), seed)?;
    }
    let raw = if key == "settings" {
        preserve_unsupported_settings(old.as_deref(), raw)
    } else {
        raw.to_string()
    };
    // Map entries are user data, not unknown schema fields: retaining absent
    // keys here would resurrect projected progress/hide deletions on restart.
    let raw = if key == "settings" {
        preserve_settings_snapshot(old.as_deref(), &raw)
    } else {
        raw
    };
    store.queue_extra(&snapshot_key, Some(raw));
    store.save()?;
    drop(store);
    if changed && !projection && !seed {
        nova_sync::request_local_sync();
    }
    Ok(())
}

fn addon_records(addons: &[AddonStore]) -> Result<Vec<(String, String, u64)>, serde_json::Error> {
    let mut records = addons
        .iter()
        .map(|addon| {
            Ok((
                addon.url.clone(),
                serde_json::to_string(&SyncedAddon {
                    capabilities: addon.capabilities,
                    label: addon.label.clone(),
                    enabled: addon.enabled,
                    configure_ok: None,
                })?,
                0,
            ))
        })
        .collect::<Result<Vec<_>, serde_json::Error>>()?;
    records.push(addon_order_record(addons));
    Ok(records)
}

/// Package selection and addon membership are one durable user action. Archive
/// bytes are staged first; a failed metadata commit leaves the active addon intact.
pub(super) fn persist_package_state(
    addons: &[AddonStore],
    packages: &std::collections::BTreeMap<String, addon_packages::PackageDescriptor>,
) -> Result<(), String> {
    let commit = || -> nova_sync::Result<()> {
        if !profiles::writable(&profiles::current_id()) {
            return Err(storage::Error::new(
                storage::ErrorKind::Schema,
                "profile has been deleted",
            )
            .into());
        }
        if !writable_key("addons") || !writable_key(addon_packages::KEY) {
            return Err(storage::Error::new(
                storage::ErrorKind::Schema,
                "unreadable addon state retained",
            )
            .into());
        }
        let addon_raw = serde_json::to_string(addons)?;
        let package_raw = serde_json::to_string(packages)?;
        let domains = [
            (DOMAIN_ADDONS, addon_records(addons)?),
            (
                DOMAIN_PACKAGES,
                addon_packages::records(&package_raw)
                    .map_err(|e| storage::Error::new(storage::ErrorKind::Schema, e))?,
            ),
        ];
        let owner = nova_sync::local_store()?;
        let dev = nova_sync::local_device()?;
        let mut store = owner.lock().unwrap();
        if store
            .record("profile_deletions", &profiles::current_id())
            .is_some_and(|r| !r.is_deleted())
        {
            return Err(storage::Error::new(
                storage::ErrorKind::Schema,
                "profile has been deleted",
            )
            .into());
        }
        // Stage the complete action away from the live owner. A failed save
        // must not leave a package selection queued for a later background retry.
        let mut candidate = store.clone();
        let mut changed = false;
        for (domain, records) in domains {
            changed |= nova_sync::prepare_snapshot(
                &mut candidate,
                dev,
                &profile_domain(domain),
                &records,
                None,
                applying(),
                false,
            )?;
        }
        candidate.queue_extra(&profiles::storage_key("addons"), Some(addon_raw));
        candidate.queue_extra(
            &profiles::storage_key(addon_packages::KEY),
            Some(package_raw),
        );
        candidate.save()?;
        *store = candidate;
        drop(store);
        if changed && !applying() {
            nova_sync::request_local_sync();
        }
        Ok(())
    };
    commit().map_err(|e| e.to_string())
}

/// Push the Continue Watching hide map (`id -> hidden_at` unix secs). A key
/// that disappears (resumed, or pruned once progress moves past it) is
/// tombstoned by `sync_records`, so "show it again" propagates mesh-wide too.
pub(crate) fn notify_continue_hidden(map: &HashMap<String, u64>) {
    let records = map
        .iter()
        .map(|(id, at)| (id.clone(), at.to_string(), *at))
        .collect();
    sync_records(DOMAIN_CONTINUE_HIDDEN, records);
}

/// Merge one episode's progress across the mesh (`local` = ours, `remote` =
/// the store's LWW winner). Plain record-LWW would let a stale unwatched
/// record clobber a watched one, so this reconciles field-by-field:
///
/// * An explicit unwatch on the newer side wins wholesale: unwatch zeroes
///   the position and stamps `unwatched_at_secs`, so it is distinguishable
///   from a stale record that merely never reached the end.
/// * Otherwise `watched` sticks: either side watched → watched, with the
///   watched side's position/duration (a coherent triple, never a mix).
/// * Both unwatched → plain recency (LWW): deliberate rewinds survive.
/// * `play_count` takes the max (monotonic, no double-count on re-sync);
///   `updated_at_secs` takes the max (Continue ordering follows activity).
/// * `None` local passes the remote record through.
///
/// The watch/unwatch paths keep the `unwatched_at_secs` invariant that makes
/// this sound: setting watched always clears it, unwatch always stamps it.
/// Ties go to local (stable, no flapping between identical merges).
#[cfg(test)]
pub(crate) fn merge_progress(
    local: Option<&EpisodeProgress>,
    remote: &EpisodeProgress,
) -> EpisodeProgress {
    let Some(local) = local else {
        return remote.clone();
    };
    let (newer, older) = if local.updated_at_secs >= remote.updated_at_secs {
        (local, remote)
    } else {
        (remote, local)
    };
    let play_count = newer.play_count.max(older.play_count);
    if !newer.watched && newer.unwatched_at_secs > older.unwatched_at_secs {
        let mut merged = newer.clone();
        merged.play_count = play_count;
        return merged;
    }
    if newer.watched || older.watched {
        let base = if newer.watched { newer } else { older };
        let mut merged = base.clone();
        merged.watched = true;
        merged.play_count = play_count;
        merged.updated_at_secs = newer.updated_at_secs;
        return merged;
    }
    let mut merged = newer.clone();
    merged.play_count = play_count;
    merged
}

pub(crate) fn notify_addons(addons: &[AddonStore]) {
    let mut records: Vec<(String, String, u64)> = addons
        .iter()
        .filter_map(|addon| {
            let entry = SyncedAddon {
                capabilities: addon.capabilities,
                label: addon.label.clone(),
                enabled: addon.enabled,
                configure_ok: None,
            };
            serde_json::to_string(&entry)
                .ok()
                .map(|json| (addon.url.clone(), json, 0))
        })
        .collect();
    // Order is inherently whole-value (a reordered list has no meaningful
    // per-item merge), so it syncs as one LWW record: concurrent reorders on
    // two devices resolve to whichever happened last. The key can't collide
    // with an addon record (those are keyed by URL).
    records.push(addon_order_record(addons));
    sync_records(DOMAIN_ADDONS, records);
}

/// The `order` record for `DOMAIN_ADDONS`: the installed URL list, whole-value
/// LWW. Unknown URLs sort after the listed ones (see `sort_by_order`).
pub(crate) const ADDON_ORDER_KEY: &str = "order";

pub(crate) fn addon_order_record(addons: &[AddonStore]) -> (String, String, u64) {
    let urls: Vec<&str> = addons.iter().map(|a| a.url.as_str()).collect();
    let json = serde_json::to_string(&urls).unwrap_or_else(|_| "[]".to_string());
    (ADDON_ORDER_KEY.to_string(), json, 0)
}

/// Order `current` urls by `order` (whole-value LWW): listed URLs first in
/// listed order; anything unknown keeps current relative order at the end.
pub(crate) fn sort_by_order(current: Vec<String>, order: &[String]) -> Vec<String> {
    let rank: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, url)| (url.as_str(), i))
        .collect();
    let mut indexed: Vec<(usize, String)> = current.into_iter().enumerate().collect();
    indexed.sort_by_key(|(pos, url)| (rank.get(url.as_str()).copied().unwrap_or(usize::MAX), *pos));
    indexed.into_iter().map(|(_, url)| url).collect()
}

/// `CacheSettings` fields that are never synced: device-specific
/// (`android_hwdec`, `desktop_hwdec`, `player_external`, `desktop_external_app`,
/// Android return behavior) or local-only
/// (device-local preferences);
/// `categories` sync as their own domain so concurrent additions union.
const UNSYNCED_SETTINGS_FIELDS: &[&str] = &[
    "android_hwdec",
    "desktop_hwdec",
    "player_external",
    "remote_control_enabled",
    "desktop_external_app",
    "android_auto_continue",
    "android_pip_enabled",
    "sync_overrides",
    "categories",
];

/// Image-cache controls are interdependent (re-encoding only applies to the
/// chosen format/quality/resolution, and re-encoding rewrites entries
/// under that config), so they sync as one whole record under `settings/cache`
/// with whole-value LWW instead of field-by-field.
const SETTINGS_GROUP_CACHE: &[&str] = &[
    "cache_images",
    "enabled",
    "format",
    "quality",
    "detail_poster_max_side",
    "detail_backdrop_max_side",
    "episode_thumbnail_max_side",
    "player_episode_backdrop_max_side",
    "lru_cache_mb",
];

// The migration bit describes this exact layout: a standalone false value
// from an untouched device must never resurrect a deliberately removed row.
const SETTINGS_GROUP_HOME_LAYOUT: &[&str] = &["home_rows", "home_recommendations_migrated"];
const RECOMMENDATION_HIDDEN_PREFIX: &str = "home_recommendation_hidden:";

fn recommendation_hidden_identity(raw: &str) -> Option<String> {
    let (type_, id): (String, String) = serde_json::from_str(raw).ok()?;
    (!type_.is_empty()
        && type_.len() <= 64
        && !type_.chars().any(char::is_control)
        && !id.is_empty()
        && id.len() <= 512
        && !id.chars().any(char::is_control))
    .then(|| watch_history::title_key(&type_, &id))
}

fn union_recommendation_hidden(settings: &mut CacheSettings, records: &[(String, String)]) {
    for (key, raw) in records {
        if let Some(identity) = key.strip_prefix(RECOMMENDATION_HIDDEN_PREFIX) {
            if serde_json::from_str::<bool>(raw).ok() == Some(true)
                && let Some(identity) = recommendation_hidden_identity(identity)
            {
                settings.home_recommendations_hidden.insert(identity);
            }
        } else if key == "home_recommendations_hidden"
            && let Ok(legacy) = serde_json::from_str::<Vec<String>>(raw)
        {
            settings.home_recommendations_hidden.extend(
                legacy
                    .iter()
                    .filter_map(|identity| recommendation_hidden_identity(identity)),
            );
        }
    }
}

/// Settings fields synced as one whole-record blob: `(record key, field names)`.
const SYNCED_SETTINGS_GROUPS: &[(&str, &[&str])] = &[
    ("cache", SETTINGS_GROUP_CACHE),
    ("home_layout", SETTINGS_GROUP_HOME_LAYOUT),
];

/// The group record that carries `field`, if any.
pub(super) fn settings_group_of(field: &str) -> Option<&'static str> {
    SYNCED_SETTINGS_GROUPS
        .iter()
        .find(|(_, fields)| fields.contains(&field))
        .map(|(key, _)| *key)
}

/// The field names carried by the group record key `record`, if it is one.
pub(super) fn settings_group_fields(record: &str) -> Option<&'static [&'static str]> {
    SYNCED_SETTINGS_GROUPS
        .iter()
        .find(|(key, _)| *key == record)
        .map(|(_, fields)| *fields)
}

/// Decompose settings into `(record key, json, 0)` records: one per ungrouped
/// top-level field, plus one object-valued record per group. Serializing the
/// struct generically means new fields sync without any wiring.
fn settings_fields(settings: &CacheSettings) -> Vec<(String, String, u64)> {
    let Ok(raw) = serde_json::to_string(settings) else {
        return Vec::new();
    };
    let old = storage::try_get_str("settings").ok().flatten();
    let raw = preserve_unsupported_settings(old.as_deref(), &raw);
    let Ok(serde_json::Value::Object(mut map)) = serde_json::from_str::<serde_json::Value>(&raw)
    else {
        return Vec::new();
    };
    // Overrides never become account records. The fallback is captured before
    // the override starts; live publication refreshes it from the account store.
    for (field, shared) in &settings.sync_overrides {
        if setting_can_override(field) {
            map.insert(field.clone(), shared.clone());
        }
    }
    let mut out = Vec::new();
    let mut groups: HashMap<&str, serde_json::Map<String, serde_json::Value>> = HashMap::new();
    let migrated_home = map
        .get("home_recommendations_migrated")
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    for (name, value) in map {
        if UNSYNCED_SETTINGS_FIELDS.contains(&name.as_str()) {
            continue;
        }
        if name == "home_recommendations_hidden" {
            // Independent grow-only records preserve concurrent dismissals.
            // The profile's materialized settings remain a plain JSON set.
            if let Some(identities) = value.as_array() {
                for identity in identities.iter().filter_map(serde_json::Value::as_str) {
                    if let Some(identity) = recommendation_hidden_identity(identity) {
                        out.push((
                            format!("{RECOMMENDATION_HIDDEN_PREFIX}{identity}"),
                            "true".into(),
                            0,
                        ));
                    }
                }
            }
            continue;
        }
        // Until this layout is edited, retain the legacy row record rather
        // than publishing a new migration decision for an untouched device.
        if SETTINGS_GROUP_HOME_LAYOUT.contains(&name.as_str()) && !migrated_home {
            if name == "home_rows"
                && let Ok(json) = serde_json::to_string(&value)
            {
                out.push((name, json, 0));
            }
            continue;
        }
        match settings_group_of(&name) {
            Some(group) => {
                groups.entry(group).or_default().insert(name, value);
            }
            None => {
                if let Ok(json) = serde_json::to_string(&value) {
                    out.push((name, json, 0));
                }
            }
        }
    }
    for (group, fields) in groups {
        if let Ok(json) = serde_json::to_string(&serde_json::Value::Object(fields)) {
            out.push((group.to_string(), json, 0));
        }
    }
    out
}

/// Override membership is user data, not unknown schema: a removed key must
/// not be resurrected by the recursive forward-compatibility merge.
fn preserve_settings_snapshot(old: Option<&str>, raw: &str) -> String {
    let merged = nova_sync::preserve_unknown(old, raw);
    let (Ok(mut merged), Ok(current)) = (
        serde_json::from_str::<serde_json::Value>(&merged),
        serde_json::from_str::<serde_json::Value>(raw),
    ) else {
        return raw.into();
    };
    merged["sync_overrides"] = current
        .get("sync_overrides")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    merged.to_string()
}

/// Only actual general-setting fields may opt out of sync.
pub(super) fn setting_can_override(field: &str) -> bool {
    !UNSYNCED_SETTINGS_FIELDS.contains(&field)
        && field != "home_recommendations_migrated"
        && field != "home_recommendations_hidden"
        && serde_json::to_value(CacheSettings::default())
            .ok()
            .is_some_and(|v| v.get(field).is_some())
}

pub(super) fn shared_setting_value(
    records: &[(String, String)],
    field: &str,
) -> Option<serde_json::Value> {
    if field == "background_style" && !records.iter().any(|(name, _)| name == field) {
        let raw = &records.iter().find(|(name, _)| name == "true_black")?.1;
        return serde_json::from_str::<bool>(raw)
            .ok()
            .map(|black| serde_json::json!(nova_config::BackgroundStyle::from_true_black(black)));
    }
    let key = settings_group_of(field).unwrap_or(field);
    let record = records.iter().find(|(name, _)| name == key);
    // Pre-recommendation peers store Home rows independently. The new record
    // wins when present; fallback keeps their saved layout available to the
    // existing per-device override control during migration.
    let raw = &record
        .or_else(|| {
            (key == "home_layout")
                .then(|| records.iter().find(|(name, _)| name == field))
                .flatten()
        })?
        .1;
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    if settings_group_of(field).is_some() && record.is_some() {
        value.get(field).cloned()
    } else {
        Some(value)
    }
}

pub(super) fn account_setting_records() -> Vec<(String, String)> {
    projection_store()
        .map(|engine| engine.records(DOMAIN_SETTINGS))
        .unwrap_or_default()
}

fn live_settings_fields(settings: &CacheSettings) -> Vec<(String, String, u64)> {
    let records = account_setting_records();
    settings_fields_for_records(settings, &records)
}

fn settings_fields_for_records(
    settings: &CacheSettings,
    records: &[(String, String)],
) -> Vec<(String, String, u64)> {
    let mut shared = settings.clone();
    retain_migrated_home_layout(&mut shared, records);
    // A full publication can precede UI projection of received records. Never
    // interpret a stale snapshot's missing identity as an unhide operation.
    union_recommendation_hidden(&mut shared, records);
    for (field, fallback) in &mut shared.sync_overrides {
        if let Some(latest) = shared_setting_value(records, field) {
            *fallback = latest;
        }
    }
    if shared.sync_overrides.contains_key("home_rows") {
        // A local-only layout edit also changes its local migration bit.
        // Publish the account layout's migration decision with its fallback.
        shared.home_recommendations_migrated =
            shared_setting_value(records, "home_recommendations_migrated")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
    }
    settings_fields(&shared)
}

fn retain_migrated_home_layout(settings: &mut CacheSettings, records: &[(String, String)]) {
    if !settings.home_recommendations_migrated
        && shared_setting_value(records, "home_recommendations_migrated")
            .and_then(|value| value.as_bool())
            == Some(true)
        && let Some(rows) = shared_setting_value(records, "home_rows")
            .and_then(|value| serde_json::from_value(value).ok())
    {
        // An untouched snapshot must neither overwrite nor tombstone the
        // migrated group already received by this device's record store.
        settings.home_rows = rows;
        settings.home_recommendations_migrated = true;
    }
}

/// Rebuild settings by overlaying records onto `base` (the local settings).
/// A group record replaces every field it carries, so the group moves as one
/// LWW value. Device-specific fields always keep the local value; unknown
/// fields are ignored by serde, so a newer peer can add settings safely.
fn merge_settings_fields(base: &CacheSettings, fields: &[(String, String)]) -> CacheSettings {
    let Ok(serde_json::Value::Object(mut map)) = serde_json::to_value(base) else {
        return base.clone();
    };
    let has_home_layout = fields.iter().any(|(name, _)| name == "home_layout");
    for (name, json) in fields {
        if UNSYNCED_SETTINGS_FIELDS.contains(&name.as_str()) {
            continue;
        }
        if name == "home_recommendations_hidden" || name.starts_with(RECOMMENDATION_HIDDEN_PREFIX) {
            continue;
        }
        if has_home_layout && SETTINGS_GROUP_HOME_LAYOUT.contains(&name.as_str()) {
            continue;
        }
        if name == "home_layout" {
            if let Ok(serde_json::Value::Object(group)) = serde_json::from_str(json)
                && SETTINGS_GROUP_HOME_LAYOUT
                    .iter()
                    .all(|field| group.contains_key(*field))
            {
                let mut trial = map.clone();
                for field in SETTINGS_GROUP_HOME_LAYOUT {
                    trial.insert((*field).into(), group[*field].clone());
                }
                if serde_json::from_value::<CacheSettings>(serde_json::Value::Object(trial.clone()))
                    .is_ok()
                {
                    map = trial;
                    for field in SETTINGS_GROUP_HOME_LAYOUT {
                        allow_setting_field(field);
                    }
                } else {
                    for field in SETTINGS_GROUP_HOME_LAYOUT {
                        protect_setting_field(field, group[*field].clone());
                    }
                    storage::report(storage::Error::new(
                        storage::ErrorKind::Schema,
                        "unsupported synced Home layout retained in record store",
                    ));
                }
            }
            continue;
        }
        // New style records take precedence over the retired boolean, even
        // when record iteration order differs between paired devices.
        if name == "true_black" {
            if !fields.iter().any(|(key, _)| key == "background_style")
                && let Ok(black) = serde_json::from_str::<bool>(json)
            {
                map.insert(
                    "background_style".into(),
                    serde_json::json!(nova_config::BackgroundStyle::from_true_black(black)),
                );
            }
            continue;
        }
        if let Some(allowed) = settings_group_fields(name) {
            if let Ok(serde_json::Value::Object(mut group)) =
                serde_json::from_str::<serde_json::Value>(json)
            {
                // Older peers grouped `downscale` in this record. The local
                // settings object already contains the new field, so make
                // the old group's migration explicit before overlaying it.
                if !group.contains_key("player_episode_backdrop_max_side")
                    && group.get("downscale").and_then(serde_json::Value::as_bool) == Some(true)
                {
                    group.insert(
                        "player_episode_backdrop_max_side".into(),
                        serde_json::json!(1024),
                    );
                }
                for (field, value) in group {
                    if allowed.contains(&field.as_str()) {
                        let mut trial = map.clone();
                        trial.insert(field.clone(), value.clone());
                        if serde_json::from_value::<CacheSettings>(serde_json::Value::Object(
                            trial.clone(),
                        ))
                        .is_ok()
                        {
                            map = trial;
                            allow_setting_field(&field);
                        } else {
                            protect_setting_field(&field, value);
                            storage::report(storage::Error::new(
                                storage::ErrorKind::Schema,
                                "unsupported synced settings field retained in record store",
                            ));
                        }
                    }
                }
            }
        } else if let Ok(value) = serde_json::from_str::<serde_json::Value>(json) {
            let mut trial = map.clone();
            trial.insert(name.clone(), value.clone());
            if serde_json::from_value::<CacheSettings>(serde_json::Value::Object(trial.clone()))
                .is_ok()
            {
                map = trial;
                allow_setting_field(name);
            } else {
                protect_setting_field(name, value);
                storage::report(storage::Error::new(
                    storage::ErrorKind::Schema,
                    "unsupported synced settings field retained in record store",
                ));
            }
        }
    }
    let local = serde_json::to_value(base).unwrap_or_default();
    let mut overrides = base.sync_overrides.clone();
    for (field, baseline) in &mut overrides {
        if !setting_can_override(field) {
            continue;
        }
        if let Some(shared) = shared_setting_value(fields, field) {
            let mut trial = local.clone();
            trial[field] = shared.clone();
            if serde_json::from_value::<CacheSettings>(trial).is_ok() {
                *baseline = shared;
            }
        }
        if let Some(effective) = local.get(field) {
            map.insert(field.clone(), effective.clone());
        }
    }
    if overrides.contains_key("home_rows") {
        map.insert(
            "home_recommendations_migrated".into(),
            serde_json::json!(base.home_recommendations_migrated),
        );
    }
    map.insert(
        "sync_overrides".into(),
        serde_json::to_value(overrides).unwrap_or_default(),
    );
    let mut merged: CacheSettings =
        serde_json::from_value(serde_json::Value::Object(map)).unwrap_or_else(|_| base.clone());
    union_recommendation_hidden(&mut merged, fields);
    // Device-specific fields are never taken from the remote record.
    merged.android_hwdec = base.android_hwdec;
    merged.desktop_hwdec = base.desktop_hwdec;
    merged.player_external = base.player_external;
    merged.remote_control_enabled = base.remote_control_enabled;
    merged.desktop_external_app = base.desktop_external_app;
    merged
}

pub(crate) fn notify_settings(settings: &CacheSettings) {
    if !writable_key("settings") {
        return;
    }
    // Independent settings sync per field so concurrent edits on different
    // devices union; the cache group syncs as one coupled record (see above).
    // `sync_records` tombstones any live settings key that is no longer
    // emitted, which retires the legacy whole-blob `core` key automatically.
    sync_records(DOMAIN_SETTINGS, live_settings_fields(settings));
    let categories = settings
        .categories
        .iter()
        .map(|name| (name.clone(), "1".to_string(), 0))
        .collect();
    sync_records(DOMAIN_CATEGORY, categories);
}

pub(crate) fn notify_setting_choice(field: &str, settings: &CacheSettings) {
    if !profiles::writable(&profiles::current_id()) {
        return;
    }
    if settings.sync_overrides.contains_key(field) {
        return;
    }
    let key = settings_group_of(field).unwrap_or(field);
    let records = live_settings_fields(settings)
        .into_iter()
        .filter(|(k, _, _)| {
            if field == "home_recommendations_hidden" {
                k.starts_with(RECOMMENDATION_HIDDEN_PREFIX)
            } else {
                k == key
            }
        })
        .collect::<Vec<_>>();
    if !records.is_empty() {
        let result = (|| -> nova_sync::Result<()> {
            let dev = nova_sync::local_device()?;
            let owner = nova_sync::local_store()?;
            let mut store = owner.lock().unwrap();
            for (key, value, _) in records {
                let value = nova_sync::preserve_unknown(
                    store
                        .record(&profile_domain(DOMAIN_SETTINGS), &key)
                        .and_then(|r| r.value.as_deref()),
                    &value,
                );
                store.set(&profile_domain(DOMAIN_SETTINGS), &key, Some(value), 0, dev);
            }
            store.save()
        })();
        if let Err(e) = result {
            storage::report(storage::Error::new(storage::ErrorKind::Transaction, e));
        }
    }
}

/// Snapshots used to seed inactive profiles come from disk, never from the
/// active profile's Shared models. The record owner still exchanges every domain.
const PROFILE_SNAPSHOTS: &[(&str, &str)] = &[
    ("library", DOMAIN_LIBRARY),
    ("episode_progress", DOMAIN_PROGRESS),
    (PREFERENCES_KEY, DOMAIN_PLAYBACK_PREFERENCES),
    ("continue_hidden", DOMAIN_CONTINUE_HIDDEN),
    (VIEWING_ACTIVITY_KEY, DOMAIN_VIEWING_ACTIVITY),
    (NEW_EPISODES_HIDDEN_KEY, DOMAIN_NEW_EPISODES_HIDDEN),
    (watch_history::HIDDEN_KEY, DOMAIN_HISTORY_HIDDEN),
    ("addons", DOMAIN_ADDONS),
    (plugin_settings::KEY, DOMAIN_PLUGIN_SETTINGS),
    (addon_packages::KEY, DOMAIN_PACKAGES),
    ("settings", DOMAIN_SETTINGS),
];

fn seed_stored_profiles() -> nova_sync::Result<()> {
    if !profiles::is_initialized() {
        return Ok(());
    }
    let owner = nova_sync::local_store()?;
    let ids = {
        let store = owner.lock().unwrap();
        let mut ids = BTreeSet::new();
        for (id, _) in store.records("profiles") {
            ids.insert(id);
        }
        for domain in store.domains() {
            if let Some((id, _)) = nova_sync::parse_profile_domain(&domain) {
                ids.insert(id.to_string());
            }
        }
        ids.retain(|id| {
            store
                .record("profile_deletions", id)
                .is_none_or(|r| r.is_deleted())
        });
        ids
    };
    for id in ids {
        profiles::with_context(profiles::context_for(&id), || -> nova_sync::Result<()> {
            for (key, domain) in PROFILE_SNAPSHOTS {
                let baseline = format!("sync:baseline:{}", profile_domain(domain));
                if nova_storage::try_get_str(&baseline)?.is_some() {
                    continue;
                }
                if let Some(raw) = nova_storage::try_get_str(&profiles::storage_key_for(&id, key))?
                {
                    persist_sync_snapshot(key, &raw, true)?;
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}

fn projected_map<T: serde::de::DeserializeOwned + Serialize>(
    records: &[(String, String)],
) -> nova_sync::Result<String> {
    let values = records
        .iter()
        .map(|(key, raw)| serde_json::from_str::<T>(raw).map(|value| (key.clone(), value)))
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?;
    Ok(serde_json::to_string(&values)?)
}

/// Stale peers may retain a value after ordinary child tombstones expire. A
/// permanent profile marker turns that late value into a newer tombstone, so
/// secrets disappear from the record owner and both version digests converge.
fn tombstone_deleted_profile_domain(
    store: &mut nova_sync::Store,
    domain: &str,
    device: u64,
) -> nova_sync::Result<bool> {
    let mut changed = false;
    for (key, _) in store.records(domain) {
        changed |= store.set(domain, &key, None, 0, device);
    }
    if let Some(basis) = store.digest().get(domain).cloned() {
        store.mark_projected(domain, &basis)?;
    } else {
        store.save()?;
    }
    Ok(changed)
}

/// Rebuild an inactive profile durably without changing UI state or process-wide
/// provider preferences. Activation reads these snapshots through the same paths
/// as startup. Raw record-only domains are consumed by their owners on activation.
fn project_inactive_profile(id: &str, domain: &str, base: &str) -> nova_sync::Result<()> {
    let owner = nova_sync::local_store()?;
    let (records, basis, deleted) = {
        let store = owner.lock().unwrap();
        (
            store.records(domain),
            store.digest().get(domain).cloned(),
            store
                .record("profile_deletions", id)
                .is_some_and(|r| !r.is_deleted()),
        )
    };
    let Some(basis) = basis else {
        return Ok(());
    };
    if deleted {
        let device = nova_sync::local_device()?;
        let changed = tombstone_deleted_profile_domain(&mut owner.lock().unwrap(), domain, device)?;
        if changed {
            nova_sync::request_local_sync();
        }
        return Ok(());
    }
    profiles::with_context(profiles::context_for(id), || -> nova_sync::Result<()> {
        persistence::register_collections();
        let snapshot: Option<(&str, String)> = match base {
            DOMAIN_LIBRARY => {
                let mut entries = records
                    .iter()
                    .map(|(_, raw)| serde_json::from_str::<LibraryEntry>(raw))
                    .collect::<Result<Vec<_>, _>>()?;
                entries.sort_by(|a, b| {
                    a.added_at_secs
                        .cmp(&b.added_at_secs)
                        .then_with(|| a.id.cmp(&b.id))
                });
                Some(("library", serde_json::to_string(&entries)?))
            }
            DOMAIN_PROGRESS => Some((
                EPISODE_PROGRESS_KEY,
                projected_map::<EpisodeProgress>(&records)?,
            )),
            DOMAIN_PLAYBACK_PREFERENCES => {
                Some((PREFERENCES_KEY, projected_map::<PreferenceValue>(&records)?))
            }
            DOMAIN_VIEWING_ACTIVITY => Some((
                VIEWING_ACTIVITY_KEY,
                projected_map::<ViewingActivity>(&records)?,
            )),
            DOMAIN_NEW_EPISODES_HIDDEN => Some((
                NEW_EPISODES_HIDDEN_KEY,
                projected_map::<Vec<String>>(&records)?,
            )),
            DOMAIN_HISTORY_HIDDEN => {
                Some((watch_history::HIDDEN_KEY, projected_map::<u64>(&records)?))
            }
            DOMAIN_CONTINUE_HIDDEN => Some(("continue_hidden", projected_map::<u64>(&records)?)),
            DOMAIN_PLUGIN_SETTINGS => Some((
                plugin_settings::KEY,
                serde_json::to_string(&plugin_settings::project(records.clone())?)?,
            )),
            DOMAIN_PACKAGES => {
                let raw = projected_map::<serde_json::Value>(&records)?;
                addon_packages::records(&raw)
                    .map_err(|e| storage::Error::new(storage::ErrorKind::Schema, e))?;
                Some((addon_packages::KEY, raw))
            }
            DOMAIN_ADDONS => {
                let order = records
                    .iter()
                    .find(|(key, _)| key == ADDON_ORDER_KEY)
                    .map(|(_, raw)| serde_json::from_str::<Vec<String>>(raw))
                    .transpose()?
                    .unwrap_or_default();
                let mut addons = records
                    .iter()
                    .filter(|(key, _)| key != ADDON_ORDER_KEY)
                    .map(|(url, raw)| {
                        let addon: SyncedAddon = serde_json::from_str(raw)?;
                        Ok(AddonStore {
                            url: url.clone(),
                            enabled: addon.enabled,
                            label: addon.label,
                            configure_ok: addon.configure_ok,
                            capabilities: addon.capabilities,
                        })
                    })
                    .collect::<Result<Vec<_>, serde_json::Error>>()?;
                let urls = sort_by_order(addons.iter().map(|a| a.url.clone()).collect(), &order);
                addons.sort_by_key(|a| urls.iter().position(|url| url == &a.url));
                Some(("addons", serde_json::to_string(&addons)?))
            }
            DOMAIN_SETTINGS | DOMAIN_CATEGORY => {
                let raw = nova_storage::try_get_str(&profiles::storage_key_for(id, "settings"))?;
                let local: CacheSettings = raw
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()?
                    .unwrap_or_default();
                let store = owner.lock().unwrap();
                let mut settings = merge_settings_fields(
                    &local,
                    &store.records(&nova_sync::profile_domain(id, DOMAIN_SETTINGS)),
                );
                settings.categories = store
                    .records(&nova_sync::profile_domain(id, DOMAIN_CATEGORY))
                    .into_iter()
                    .map(|(key, _)| key)
                    .collect();
                Some((
                    "settings",
                    preserve_settings_snapshot(raw.as_deref(), &serde_json::to_string(&settings)?),
                ))
            }
            _ => None,
        };
        let dev = nova_sync::local_device()?;
        let mut store = owner.lock().unwrap();
        // Retirement can arrive while this snapshot is being decoded.
        if store
            .record("profile_deletions", id)
            .is_some_and(|r| !r.is_deleted())
        {
            let changed = tombstone_deleted_profile_domain(&mut store, domain, dev)?;
            drop(store);
            if changed {
                nova_sync::request_local_sync();
            }
            return Ok(());
        }
        if let Some((key, raw)) = snapshot {
            let baseline_records: Vec<_> = records
                .iter()
                .map(|(key, value)| (key.clone(), value.clone(), 0))
                .collect();
            nova_sync::prepare_snapshot(
                &mut store,
                dev,
                domain,
                &baseline_records,
                None,
                true,
                false,
            )?;
            store.queue_extra(&profiles::storage_key_for(id, key), Some(raw));
        }
        store.mark_projected(domain, &basis)
    })
}

// ---------------------------------------------------------------------------
// Materialize: rebuild app state from remote records
// ---------------------------------------------------------------------------

impl Bridge {
    /// Push all current local state into the sync store. Called once at
    /// startup/after enabling; a no-op for records that already match.
    pub(super) fn sync_seed(&self) {
        // Check the durable baseline before cloning/serializing large histories.
        // Local writes already commit snapshots atomically with sync records.
        for (key, domain) in [
            ("library", DOMAIN_LIBRARY),
            ("episode_progress", DOMAIN_PROGRESS),
            (PREFERENCES_KEY, DOMAIN_PLAYBACK_PREFERENCES),
            ("continue_hidden", DOMAIN_CONTINUE_HIDDEN),
            (VIEWING_ACTIVITY_KEY, DOMAIN_VIEWING_ACTIVITY),
            (NEW_EPISODES_HIDDEN_KEY, DOMAIN_NEW_EPISODES_HIDDEN),
            (watch_history::HIDDEN_KEY, DOMAIN_HISTORY_HIDDEN),
            ("addons", DOMAIN_ADDONS),
            (plugin_settings::KEY, DOMAIN_PLUGIN_SETTINGS),
            (addon_packages::KEY, DOMAIN_PACKAGES),
            ("settings", DOMAIN_SETTINGS),
        ] {
            let result = (|| -> nova_sync::Result<()> {
                if storage::try_get_str(&format!("sync:baseline:{}", profile_domain(domain)))?
                    .is_some()
                {
                    return Ok(());
                }
                let state = self.shared.lock().unwrap();
                let raw = match key {
                    plugin_settings::KEY | addon_packages::KEY => {
                        storage::try_get_str(key)?.unwrap_or_else(|| "{}".into())
                    }
                    "library" => serde_json::to_string(&state.entries)?,
                    "episode_progress" => serde_json::to_string(&state.progress)?,
                    PREFERENCES_KEY => serde_json::to_string(
                        &read_json::<HashMap<String, PreferenceValue>>(PREFERENCES_KEY)
                            .unwrap_or_default(),
                    )?,
                    "continue_hidden" => serde_json::to_string(&state.continue_hidden)?,
                    "addons" => serde_json::to_string(
                        &state
                            .installed
                            .iter()
                            .map(|a| AddonStore {
                                capabilities: nova_providers::provider_capabilities(&a.url),
                                url: a.url.clone(),
                                enabled: a.enabled,
                                configure_ok: a.configure_ok,
                                label: a.label.clone(),
                            })
                            .collect::<Vec<_>>(),
                    )?,
                    "settings" => serde_json::to_string(&state.cache_settings)?,
                    watch_history::HIDDEN_KEY => serde_json::to_string(
                        &read_json::<HashMap<String, u64>>(key).unwrap_or_default(),
                    )?,
                    VIEWING_ACTIVITY_KEY => serde_json::to_string(&read_viewing_activity())?,
                    NEW_EPISODES_HIDDEN_KEY => serde_json::to_string(
                        &read_json::<HashMap<String, Vec<String>>>(key).unwrap_or_default(),
                    )?,
                    _ => unreachable!(),
                };
                drop(state);
                persist_sync_snapshot(key, &raw, true)
            })();
            if let Err(e) = result {
                storage::report(storage::Error::new(storage::ErrorKind::Transaction, e));
            }
        }
        if let Err(error) = seed_stored_profiles() {
            storage::report(storage::Error::new(storage::ErrorKind::Transaction, error));
        }
    }

    /// Upgrade an existing settings baseline with newly supported defaults.
    pub(super) fn seed_playback_default_settings(&self) {
        if !writable_key("settings") {
            return;
        }
        let settings = self.shared.lock().unwrap().cache_settings.clone();
        let stored: serde_json::Value = storage::try_get_str("settings")
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        let fields: Vec<_> = live_settings_fields(&settings)
            .into_iter()
            .filter(|(key, _, _)| {
                matches!(
                    key.as_str(),
                    "playback_speed"
                        | "default_audio_language"
                        | "default_subtitle_language"
                        | "subtitle_style"
                )
            })
            .collect();
        let result = (|| -> nova_sync::Result<()> {
            let owner = nova_sync::local_store()?;
            let dev = nova_sync::local_device()?;
            let mut store = owner.lock().unwrap();
            let baseline: std::collections::BTreeMap<String, String> = store
                .extra_value(&format!(
                    "sync:baseline:{}",
                    profile_domain(DOMAIN_SETTINGS)
                ))?
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
            let mut changed = false;
            for (key, value, _) in fields {
                // Existing projection-only defaults are not user edits. Only
                // backfill a legacy field absent from its materialized baseline.
                if stored.get(&key).is_some()
                    && !baseline.contains_key(&key)
                    && store
                        .record(&profile_domain(DOMAIN_SETTINGS), &key)
                        .is_none()
                {
                    changed |=
                        store.set(&profile_domain(DOMAIN_SETTINGS), &key, Some(value), 0, dev);
                }
            }
            if changed {
                store.save()?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            storage::report(storage::Error::new(storage::ErrorKind::Transaction, error));
        }
        // An older build may already have acknowledged these records while
        // treating speed as device-local. Re-read just the newly supported
        // fields even when their projection digest was previously acknowledged.
        let records: Vec<_> = account_setting_records()
            .into_iter()
            .filter(|(key, _)| {
                matches!(
                    key.as_str(),
                    "playback_speed"
                        | "default_audio_language"
                        | "default_subtitle_language"
                        | "subtitle_style"
                )
            })
            .collect();
        let settings = {
            let mut state = self.shared.lock().unwrap();
            state.cache_settings = merge_settings_fields(&state.cache_settings, &records);
            state.cache_settings.clone()
        };
        set_active_cache_settings(settings);
    }

    /// Older apps acknowledge unfamiliar domains without materializing them.
    /// Import a stored preference domain when upgrading such a device.
    pub(super) fn restore_playback_preferences_schema(&self) {
        if storage::try_get_str(PREFERENCES_KEY)
            .ok()
            .flatten()
            .is_none()
            && projection_store()
                .is_some_and(|store| !store.records(DOMAIN_PLAYBACK_PREFERENCES).is_empty())
        {
            self.sync_apply_inner(vec![DOMAIN_PLAYBACK_PREFERENCES.into()], false);
        }
    }

    pub(super) fn replay_sync_projection(&self) {
        self.replay_sync_projection_inner(true);
    }

    /// Restore unapplied headless changes before deriving the first UI models.
    pub(super) fn restore_sync_projection(&self) {
        self.replay_sync_projection_inner(false);
    }

    fn replay_sync_projection_inner(&self, publish_models: bool) {
        if let Ok(owner) = nova_sync::local_store() {
            let pending = {
                let mut store = owner.lock().unwrap();
                if let Err(e) = store.save() {
                    storage::report(storage::Error::new(storage::ErrorKind::Transaction, e));
                    return;
                }
                store.pending_domains()
            };
            if !pending.is_empty() {
                self.sync_apply_inner(pending, publish_models);
            }
        }
    }

    /// Apply remote records for the given domains. Runs on the UI thread.
    pub(super) fn sync_apply(&self, domains: Vec<String>) {
        self.sync_apply_inner(domains, true);
    }

    fn sync_apply_inner(&self, domains: Vec<String>, publish_models: bool) {
        if domains.is_empty() {
            return;
        }
        // Reconcile retirement before waiting on the ordered writer: a late
        // snapshot for a remotely deleted profile must be discarded rather
        // than keep this account-wide queue blocked behind a schema error.
        if domains
            .iter()
            .any(|domain| matches!(domain.as_str(), "profiles" | "profile_deletions"))
            && let Err(error) = profiles::list()
        {
            io::record_write_failure(storage::Error::new(storage::ErrorKind::Transaction, error));
            return;
        }
        if let Err(error) = persistence::flush() {
            io::record_write_failure(storage::Error::new(storage::ErrorKind::Transaction, error));
            return;
        }
        // Flush a slider edit before projecting a remote snapshot; independent
        // fields then merge in the record owner instead of losing the local edit.
        self.flush_playback_preferences(true);
        if domains
            .iter()
            .any(|domain| matches!(domain.as_str(), "profiles" | "profile_deletions"))
        {
            self.refresh_profiles();
        }
        let active = profiles::current_id();
        let deleted_profiles: HashSet<String> = nova_sync::local_store()
            .ok()
            .map(|owner| {
                owner
                    .lock()
                    .unwrap()
                    .records("profile_deletions")
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect()
            })
            .unwrap_or_default();
        let mut active_domains = Vec::new();
        for domain in domains {
            if let Some((id, base)) = nova_sync::parse_profile_domain(&domain) {
                if id != active || deleted_profiles.contains(id) {
                    if let Err(error) = project_inactive_profile(id, &domain, base) {
                        storage::report(storage::Error::new(
                            storage::ErrorKind::Transaction,
                            error,
                        ));
                    }
                    continue;
                }
                active_domains.push(base.to_string());
            } else {
                active_domains.push(domain);
            }
        }
        let domains = active_domains;
        let library_changed = domains.iter().any(|d| d == DOMAIN_LIBRARY);
        let models_changed = domains.iter().any(|d| {
            matches!(
                d.as_str(),
                DOMAIN_LIBRARY
                    | DOMAIN_PROGRESS
                    | DOMAIN_CONTINUE_HIDDEN
                    | DOMAIN_VIEWING_ACTIVITY
                    | DOMAIN_NEW_EPISODES_HIDDEN
                    | DOMAIN_HISTORY_HIDDEN
                    | DOMAIN_SETTINGS
                    | DOMAIN_CATEGORY
            )
        });
        let _guard = ApplyingGuard::new();
        let image_revision = image_settings_revision();
        let peers_changed = domains.iter().any(|d| d == nova_sync::DOMAIN_PEERS);
        for domain in domains {
            let failures = write_failures();
            let basis = nova_sync::local_store().ok().and_then(|s| {
                s.lock()
                    .unwrap()
                    .digest()
                    .get(&profile_domain(&domain))
                    .cloned()
            });
            match domain.as_str() {
                stremio::DOMAIN => {
                    self.stremio_wake();
                    continue;
                }
                list_services::DOMAIN => {
                    self.list_services_wake();
                    continue;
                }
                DOMAIN_LIBRARY => self.sync_apply_library(),
                DOMAIN_PROGRESS => self.sync_apply_progress(),
                DOMAIN_PLAYBACK_PREFERENCES => self.sync_apply_json_map::<PreferenceValue>(
                    DOMAIN_PLAYBACK_PREFERENCES,
                    PREFERENCES_KEY,
                ),
                DOMAIN_LAST_STREAM => {
                    if let Err(error) = self.project_last_stream_choice() {
                        storage::report(storage::Error::new(storage::ErrorKind::Schema, error));
                        continue;
                    }
                    if publish_models
                        && self.app().is_some_and(|app| app.get_stream_selector_open())
                    {
                        self.apply_stream_filter();
                    }
                }
                DOMAIN_VIEWING_ACTIVITY => self.sync_apply_viewing_activity(),
                DOMAIN_NEW_EPISODES_HIDDEN => self.sync_apply_new_episodes_hidden(),
                DOMAIN_HISTORY_HIDDEN => self
                    .sync_apply_json_map::<u64>(DOMAIN_HISTORY_HIDDEN, watch_history::HIDDEN_KEY),
                DOMAIN_ADDONS => self.sync_apply_addons(),
                DOMAIN_PACKAGES => {
                    let result = (|| -> Result<(), String> {
                        let Some(store) = projection_store() else {
                            return Ok(());
                        };
                        let records: std::collections::BTreeMap<String, serde_json::Value> = store
                            .records(DOMAIN_PACKAGES)
                            .into_iter()
                            .map(|(url, raw)| {
                                serde_json::from_str(&raw)
                                    .map(|value| (url, value))
                                    .map_err(|e| e.to_string())
                            })
                            .collect::<Result<_, _>>()?;
                        let raw = serde_json::to_string(&records).map_err(|e| e.to_string())?;
                        addon_packages::project(self, &raw)
                    })();
                    if let Err(error) = result {
                        storage::report(storage::Error::new(storage::ErrorKind::Schema, error));
                        continue;
                    }
                }
                DOMAIN_PLUGIN_SETTINGS => {
                    let result = (|| -> nova_sync::Result<()> {
                        let Some(store) = projection_store() else {
                            return Ok(());
                        };
                        nova_providers::plugin_settings::apply_settings_projection(|| {
                            let values =
                                plugin_settings::project(store.records(DOMAIN_PLUGIN_SETTINGS))?;
                            serde_json::to_string(&values).map_err(|error| {
                                nova_providers::ProviderHostError(error.to_string())
                            })
                        })
                        .map_err(|e| storage::Error::new(storage::ErrorKind::Schema, e))?;
                        if publish_models
                            && let Some(app) = self.app().filter(|app| app.get_addon_config_open())
                        {
                            self.refresh_plugin_settings_ui(&app.get_addon_config_url());
                        }
                        Ok(())
                    })();
                    if let Err(error) = result {
                        storage::report(storage::Error::new(
                            storage::ErrorKind::Transaction,
                            error,
                        ));
                        continue;
                    }
                }
                DOMAIN_SETTINGS | DOMAIN_CATEGORY => self.sync_apply_settings(publish_models),
                DOMAIN_CONTINUE_HIDDEN => self.sync_apply_continue_hidden(),
                nova_tracking::peer::TRACKING_DOMAIN => {
                    // The actor commits tracking state/credentials and acknowledges
                    // the captured digest only after its asynchronous apply succeeds.
                    self.tracking_sync_apply();
                    continue;
                }
                nova_tracking::TRACKING_COMPLETION_DOMAIN => {
                    self.tracking_completion_sync_apply();
                    continue;
                }
                _ => {}
            }
            if let (Ok(owner), Some(basis)) = (
                nova_sync::local_store(),
                basis.filter(|_| write_failures() == failures),
            ) {
                if let Err(e) = owner
                    .lock()
                    .unwrap()
                    .mark_projected(&profile_domain(&domain), &basis)
                {
                    storage::report(storage::Error::new(storage::ErrorKind::Transaction, e));
                } else if tracing::enabled!(target: "nova_sync::projection", tracing::Level::DEBUG)
                {
                    let revision = basis.values().fold(nova_sync::Hlc::default(), |clock, v| {
                        let next = v.hlc();
                        if next.newer_than(clock) { next } else { clock }
                    });
                    tracing::debug!(target: "nova_sync::projection", basis_ms = revision.physical_ms,
                        basis_counter = revision.counter, record_count = basis.len(), "projection finished");
                }
            }
        }
        addon_packages::reconcile(self);
        // Publish once after the whole batch, so every surface sees the same
        // merged state. Startup publishes its initial models after this replay.
        if publish_models && models_changed {
            self.sync_reconcile_progress_aliases();
            if library_changed {
                self.apply_library_to_ui();
            }
            self.apply_episode_rows();
            self.refresh_library_progress_ui();
        }
        if publish_models {
            self.refresh_remote_artwork_if_revision_changed(image_revision);
        }
        if peers_changed {
            // The mesh grew or shrank: refresh the list and immediately run a
            // pass so newly learned devices are dialed now. `Notify` coalesces
            // these, so calling it directly cannot storm.
            self.sync_status_to_ui();
            if let Some(engine) = nova_sync::engine() {
                // A peer removed us from its sync: say so instead of silently
                // dropping it from the list.
                if let Some(name) = engine.take_removed_notice() {
                    let name = if name.trim().is_empty() {
                        None
                    } else {
                        Some(name)
                    };
                    self.set_sync_notice(SyncNotice::RemovedFromSync(name));
                }
                engine.sync_now();
            }
        }
    }

    /// Remote recipe edits can arrive while any page is open. Rebuild its
    /// request identities once after the batch, before old workers deliver;
    /// progress-only batches retain their lightweight projection path.
    pub(super) fn refresh_remote_artwork_if_revision_changed(&self, previous_revision: u64) {
        if previous_revision == image_settings_revision() {
            return;
        }
        let Some(app) = self.app() else { return };
        if app.get_show_calendar() {
            self.apply_calendar_to_ui();
        }
        if app.get_show_downloads() {
            self.refresh_downloads_page();
        }
        if app.get_modal_visible() {
            self.apply_season_cards();
            self.refresh_episode_rows();
            let urls = self.shared.lock().unwrap().modal_item.as_ref().map(|item| {
                (
                    item.id.clone(),
                    item.poster_url.clone(),
                    item.background_url.clone(),
                )
            });
            self.load_current_detail_logo();
            if let Some((id, poster, backdrop)) = urls {
                if !poster.is_empty() {
                    self.load_detail_poster(poster, id.clone());
                }
                if !backdrop.is_empty() {
                    self.load_detail_backdrop(backdrop, id);
                }
            }
        }
    }

    fn sync_apply_library(&self) {
        let Some(engine) = projection_store() else {
            return;
        };
        let mut entries: Vec<LibraryEntry> = engine
            .records(DOMAIN_LIBRARY)
            .iter()
            .filter_map(|(_, v)| serde_json::from_str(v).ok())
            .collect();
        entries.sort_by(|a, b| {
            a.added_at_secs
                .cmp(&b.added_at_secs)
                .then_with(|| a.id.cmp(&b.id))
        });
        {
            self.shared.lock().unwrap().entries = entries.clone();
        }
        // Guarded write: keeps the local KV snapshot in step with the store.
        write_persisted_library(&entries);
    }

    fn sync_apply_new_episodes_hidden(&self) {
        self.sync_apply_json_map::<Vec<String>>(
            DOMAIN_NEW_EPISODES_HIDDEN,
            NEW_EPISODES_HIDDEN_KEY,
        );
    }

    fn sync_apply_viewing_activity(&self) {
        self.sync_apply_json_map::<ViewingActivity>(DOMAIN_VIEWING_ACTIVITY, VIEWING_ACTIVITY_KEY);
    }

    fn sync_apply_json_map<T: serde::de::DeserializeOwned + Serialize>(
        &self,
        domain: &str,
        key: &str,
    ) {
        let Some(engine) = projection_store() else {
            return;
        };
        let values = engine
            .records(domain)
            .into_iter()
            .map(|(id, raw)| serde_json::from_str::<T>(&raw).map(|value| (id, value)))
            .collect::<Result<HashMap<_, _>, _>>();
        match values {
            Ok(values) => write_json(key, &values),
            Err(error) => {
                storage::report(storage::Error::new(storage::ErrorKind::Schema, error));
                block_unreadable(key);
                // Count the failed projection without replacing either the
                // original snapshot or the offending sync record.
                write_json(key, &HashMap::<String, T>::new());
            }
        }
    }

    fn sync_apply_progress(&self) {
        let Some(engine) = projection_store() else {
            return;
        };
        let live = engine.records(DOMAIN_PROGRESS);
        // Reconcile per key instead of replacing wholesale: plain record-LWW
        // would let a stale unwatched record clobber a watched one (see
        // `merge_progress`). Garbage values are skipped, as before.
        let mut merged_map: HashMap<String, EpisodeProgress> = HashMap::new();
        for (key, value) in &live {
            let Ok(remote): Result<EpisodeProgress, _> = serde_json::from_str(value) else {
                continue;
            };
            merged_map.insert(key.clone(), remote);
        }
        {
            self.shared.lock().unwrap().progress = merged_map.clone();
        }
        write_progress_map(&merged_map);
    }

    /// Metadata caches can retain different native IDs on different devices.
    /// Materialize proven aliases through the same action registers used by
    /// sync, rather than inventing another watched action or guessing by S/E.
    pub(super) fn sync_reconcile_progress_aliases(&self) -> bool {
        let mut identities = {
            let state = self.shared.lock().unwrap();
            if state.progress.is_empty() {
                return false;
            }
            state
                .entries
                .iter()
                .filter(|entry| entry.type_ != "movie")
                .map(|entry| (entry.type_.clone(), entry.id.clone()))
                .collect::<HashSet<_>>()
        };
        if let Some(item) = self.shared.lock().unwrap().modal_item.as_ref()
            && item.type_ != "movie"
        {
            identities.insert((item.type_.clone(), item.id.clone()));
        }
        let groups = identities
            .into_iter()
            .flat_map(|(type_, series)| {
                let videos = read_episodes_cache_for(&type_, &series).unwrap_or_default();
                episode_progress_alias_groups(&videos)
                    .into_iter()
                    .map(move |(native, aliases)| (series.clone(), native, aliases))
            })
            .collect::<Vec<_>>();
        if groups.is_empty() {
            return false;
        }
        // A local watch/unwatch queued before metadata arrived must be visible
        // to the resolver. Ordinary playback saves remain asynchronous.
        if let Err(error) = persistence::flush() {
            io::record_write_failure(storage::Error::new(storage::ErrorKind::Transaction, error));
            return false;
        }
        let Ok(owner) = nova_sync::local_store() else {
            return false;
        };
        let changes = {
            let store = owner.lock().unwrap();
            groups
                .into_iter()
                .filter_map(|(series, native, aliases)| {
                    let key = progress_map_key(&series, &native);
                    // Forgetting this native episode must not resurrect it via an
                    // older alias which a peer still retains.
                    if store
                        .record(&profile_domain(DOMAIN_PROGRESS), &key)
                        .is_some_and(|r| r.is_deleted())
                    {
                        return None;
                    }
                    let keys = aliases
                        .iter()
                        .filter_map(|alias| {
                            let key = progress_map_key(&series, alias);
                            let raw = store
                                .record(&profile_domain(DOMAIN_PROGRESS), &key)?
                                .value
                                .as_deref()?;
                            let p: EpisodeProgress = serde_json::from_str(raw).ok()?;
                            (p.series_id == series && p.episode_id == *alias).then_some(key)
                        })
                        .collect::<Vec<_>>();
                    let raw = store.resolved_progress_in(
                        &profile_domain(DOMAIN_PROGRESS),
                        keys.iter().map(String::as_str),
                    )?;
                    let mut progress: EpisodeProgress = serde_json::from_str(&raw).ok()?;
                    progress.series_id = series;
                    progress.episode_id = native;
                    Some((key, progress))
                })
                .collect::<Vec<_>>()
        };
        let snapshot = {
            let mut state = self.shared.lock().unwrap();
            let mut changed = false;
            for (key, progress) in changes {
                if state.progress.get(&key) != Some(&progress) {
                    state.progress.insert(key, progress);
                    changed = true;
                }
            }
            changed.then(|| state.progress.clone())
        };
        let Some(snapshot) = snapshot else {
            return false;
        };
        // Advance the materialized baseline as a projection. A later unrelated
        // checkpoint must not publish this view as a synthetic watched action.
        let _guard = ApplyingGuard::new();
        write_progress_map(&snapshot);
        true
    }

    /// Apply remote Continue Watching removals. The store already resolved
    /// per-key LWW, so its live records replace ours (a key the store
    /// tombstoned — resumed elsewhere, or pruned — disappears from ours too).
    /// A key the user removed while sync was off has no store record at all:
    /// that one is kept and published, exactly like `progress`.
    fn sync_apply_continue_hidden(&self) {
        let Some(engine) = projection_store() else {
            return;
        };
        let mut map: HashMap<String, u64> = HashMap::new();
        for (id, value) in engine.records(DOMAIN_CONTINUE_HIDDEN) {
            if let Ok(hidden_at) = value.parse::<u64>() {
                map.insert(id, hidden_at);
            }
        }
        // Local-only stamps (removed with sync off / before the domain
        // existed): keep and push. When both sides have the id, `records` is
        // already the LWW winner, so the store value stays.
        self.shared.lock().unwrap().continue_hidden = map.clone();
        // Persist locally; the ApplyingGuard keeps this from echoing back, so
        // the local-only stamps are published explicitly below.
        write_continue_hidden(&map);
    }

    fn sync_apply_addons(&self) {
        let Some(engine) = projection_store() else {
            return;
        };
        let live = engine.records(DOMAIN_ADDONS);
        // Whole-value order record (absent from older peers: keep local order).
        let order: Option<Vec<String>> = live
            .iter()
            .find(|(key, _)| key == ADDON_ORDER_KEY)
            .and_then(|(_, value)| serde_json::from_str(value).ok());
        let desired: Vec<AddonStore> = live
            .iter()
            .filter(|(url, _)| url.as_str() != ADDON_ORDER_KEY)
            .filter_map(|(url, value)| {
                serde_json::from_str::<SyncedAddon>(value)
                    .ok()
                    .map(|a| AddonStore {
                        capabilities: a.capabilities,
                        url: url.clone(),
                        enabled: a.enabled,
                        configure_ok: a.configure_ok,
                        label: a.label,
                    })
            })
            .collect();

        // Remove addons no longer present in the synced set.
        loop {
            let stale = {
                let state = self.shared.lock().unwrap();
                state
                    .installed
                    .iter()
                    .position(|a| !desired.iter().any(|d| d.url == a.url))
            };
            match stale {
                Some(idx) => self.remove_addon_at(idx),
                None => break,
            }
        }
        // Install missing ones (uses the cached manifest, else fetches) with
        // the synced label.
        for addon in &desired {
            // Older peers don't know this field. An unrelated label/enable
            // edit must not silently re-enable a locally disabled contribution.
            let capabilities = merged_addon_capabilities(
                addon.capabilities,
                nova_providers::provider_capabilities(&addon.url),
            );
            self.apply_provider_capabilities(&addon.url, capabilities, false);
            let present = self
                .shared
                .lock()
                .unwrap()
                .installed
                .iter()
                .any(|a| a.url == addon.url);
            if !present {
                self.install_persisted(
                    &addon.url,
                    addon.enabled,
                    addon.configure_ok,
                    Some(addon.label.clone()),
                );
            }
        }
        // Adopt synced labels and align enabled flags.
        for addon in &desired {
            let idx = self
                .shared
                .lock()
                .unwrap()
                .installed
                .iter()
                .position(|a| a.url == addon.url);
            let Some(idx) = idx else { continue };
            {
                let mut state = self.shared.lock().unwrap();
                if let Some(entry) = state.installed.get_mut(idx)
                    && !addon.label.trim().is_empty()
                    && entry.label != addon.label
                {
                    entry.label = addon.label.clone();
                }
            }
            let differs = {
                let state = self.shared.lock().unwrap();
                state
                    .installed
                    .get(idx)
                    .is_some_and(|a| a.enabled != addon.enabled)
            };
            if differs {
                self.toggle_addon(idx);
            }
        }
        // Resolve any residual label collisions identically on every device.
        let labels_changed = self.normalize_addon_labels();
        // Adopt the mesh order (whole-value LWW). Runs after install/remove
        // so every desired URL is present; unknown locals keep trailing.
        if let Some(order) = order {
            let sorted = {
                let state = self.shared.lock().unwrap();
                sort_by_order(
                    state.installed.iter().map(|a| a.url.clone()).collect(),
                    &order,
                )
            };
            let changed = {
                let mut state = self.shared.lock().unwrap();
                let current: Vec<String> = state.installed.iter().map(|a| a.url.clone()).collect();
                if current == sorted {
                    false
                } else {
                    let chosen_url = (state.chosen_addon != usize::MAX)
                        .then(|| {
                            state
                                .installed
                                .get(state.chosen_addon)
                                .map(|a| a.url.clone())
                        })
                        .flatten();
                    let mut by_url: HashMap<String, Installed> = state
                        .installed
                        .drain(..)
                        .map(|a| (a.url.clone(), a))
                        .collect();
                    state.installed = sorted
                        .into_iter()
                        .filter_map(|url| by_url.remove(&url))
                        .collect();
                    if let Some(url) = chosen_url {
                        state.chosen_addon = state
                            .installed
                            .iter()
                            .position(|a| a.url == url)
                            .unwrap_or(usize::MAX);
                    }
                    true
                }
            };
            if changed {
                self.refresh_all(true);
            }
        }
        self.persist_installed();
        let snapshot: Vec<AddonStore> = self
            .shared
            .lock()
            .unwrap()
            .installed
            .iter()
            .map(|addon| AddonStore {
                capabilities: desired
                    .iter()
                    .find(|d| d.url == addon.url)
                    .and_then(|d| d.capabilities)
                    .or_else(|| nova_providers::provider_capabilities(&addon.url))
                    .or_else(|| {
                        read_persisted_addons()
                            .into_iter()
                            .find(|old| old.url == addon.url)
                            .and_then(|old| old.capabilities)
                    }),
                url: addon.url.clone(),
                enabled: addon.enabled,
                configure_ok: addon.configure_ok,
                label: addon.label.clone(),
            })
            .collect();
        write_persisted_addons(&snapshot);
        self.apply_addon_rows();
        if labels_changed {
            // The apply guard suppresses the normal notify hook; re-publish the
            // corrected labels so they reach the rest of the mesh.
            let addons: Vec<AddonStore> = {
                let state = self.shared.lock().unwrap();
                state
                    .installed
                    .iter()
                    .map(|a| AddonStore {
                        capabilities: nova_providers::provider_capabilities(&a.url),
                        url: a.url.clone(),
                        enabled: a.enabled,
                        configure_ok: a.configure_ok,
                        label: a.label.clone(),
                    })
                    .collect()
            };
            notify_addons(&addons);
        }
    }

    fn sync_apply_settings(&self, publish_models: bool) {
        let Some(engine) = projection_store() else {
            return;
        };
        let local = self.shared.lock().unwrap().cache_settings.clone();
        let mut settings = merge_settings_fields(&local, &engine.records(DOMAIN_SETTINGS));
        let categories: BTreeSet<String> = engine
            .records(DOMAIN_CATEGORY)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        settings.categories = categories.into_iter().collect();

        {
            self.shared.lock().unwrap().cache_settings = settings.clone();
        }
        set_active_cache_settings(settings.clone());
        self.tracking_completion_preferences(settings.completion_rating_prompts_enabled);
        write_settings(&settings);
        if publish_models {
            self.settings_to_ui();
        }
    }
}

/// Only fresh, one-to-one metadata proof may join histories. Caption/number
/// matches and stale or multiply claimed aliases are deliberately ignored.
fn episode_progress_alias_groups(videos: &[Video]) -> Vec<(String, Vec<String>)> {
    let groups = videos
        .iter()
        .map(|video| {
            let mut ids = confirmed_episode_stream_ids(video);
            ids.push(video.id.clone());
            ids.sort();
            ids.dedup();
            (video.id.clone(), ids)
        })
        .collect::<Vec<_>>();
    let mut claims: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (native, ids) in &groups {
        for id in ids {
            claims.entry(id).or_default().insert(native);
        }
    }
    groups
        .iter()
        .filter_map(|(native, ids)| {
            let ids = ids
                .iter()
                .filter(|id| claims[id.as_str()].len() == 1)
                .cloned()
                .collect::<Vec<_>>();
            (ids.len() > 1 && ids.contains(native)).then(|| (native.clone(), ids))
        })
        .collect()
}

/// One-time backfill of [`LibraryEntry::added_at_secs`] for entries saved
/// before sync existed, using their saved order (sub-second-safe: distinct
/// ascending values). Returns true when anything changed.
pub(crate) fn backfill_added_at(entries: &mut [LibraryEntry]) -> bool {
    let mut changed = false;
    for (i, entry) in entries.iter_mut().enumerate() {
        if entry.added_at_secs == 0 {
            entry.added_at_secs = i as u64 + 1;
            changed = true;
        }
    }
    changed
}

/// Start the sync engine (startup or Settings toggle-on). No-op when already
/// running.
impl Bridge {
    pub(super) fn start_sync(&self) {
        ensure_device_name();
        #[cfg(feature = "desktop")]
        {
            if let Some(engine) = nova_sync::engine() {
                self.attach_sync(engine);
                return;
            }
            if self.sync_starting.swap(true, Ordering::AcqRel) {
                return;
            }
            let generation = self.sync_start_generation.fetch_add(1, Ordering::AcqRel) + 1;
            let b = self.clone();
            std::thread::spawn(move || {
                let result = nova_sync::SyncEngine::setup();
                let _ = profiles::invoke_account(move || {
                    if b.sync_start_generation.load(Ordering::Acquire) != generation {
                        // Runtime teardown can wait for worker shutdown too.
                        std::thread::spawn(move || drop(result));
                        return;
                    }
                    b.sync_starting.store(false, Ordering::Release);
                    if !nova_sync::read_settings().enabled || b.app().is_none() {
                        std::thread::spawn(move || drop(result));
                        return;
                    }
                    match result {
                        Ok(engine) => {
                            if nova_sync::engine().is_none() {
                                nova_sync::install(engine);
                            } else {
                                std::thread::spawn(move || drop(engine));
                            }
                            if let Some(engine) = nova_sync::engine() {
                                b.attach_sync(engine);
                            }
                        }
                        Err(e) => {
                            tracing::error!(target: "nova_sync::app", error = %format_args!("{e:#}"), "could not start sync");
                            b.sync_status_to_ui();
                        }
                    }
                });
            });
        }
        #[cfg(not(feature = "desktop"))]
        match nova_sync::foreground_engine() {
            Ok(engine) => self.attach_sync(engine),
            Err(e) => {
                tracing::error!(target: "nova_sync::app", error = %format_args!("{e:#}"), "could not start sync");
                self.sync_status_to_ui();
            }
        }
    }

    fn attach_sync(&self, engine: Arc<nova_sync::SyncEngine>) {
        self.attach_playback_service(&engine);
        // Remote changes arrive on the tokio runtime thread; marshal
        // them onto the UI thread before touching app state.
        let b = self.clone();
        engine.set_on_remote(std::sync::Arc::new(move |domains| {
            let b = b.clone();
            let _ = profiles::invoke_account(move || b.sync_apply(domains));
        }));
        // Pairing events (requests + completions) likewise marshal.
        let b = self.clone();
        engine.set_pair_callback(std::sync::Arc::new(move |event| {
            let b = b.clone();
            let _ = profiles::invoke_account(move || b.sync_pair_event(event));
        }));
        // Keep the Android periodic job in step with the enable +
        // background flags: it runs a bounded pass with no Activity.
        #[cfg(target_os = "android")]
        crate::app::android_bg::set_periodic_sync(nova_sync::read_settings().background_enabled);
        self.replay_sync_projection();
        self.sync_status_to_ui();
        if let Some(engine) = nova_sync::engine() {
            engine.sync_now();
        }
    }

    /// Stop the sync engine and drop its endpoint.
    pub(super) fn stop_sync(&self) {
        self.remote_disconnect();
        {
            let mut state = self.shared.lock().unwrap();
            for cancel in state.package_fetches.values() {
                cancel.store(true, Ordering::Release);
            }
            state.package_fetches.clear();
        }
        #[cfg(feature = "desktop")]
        {
            self.sync_start_generation.fetch_add(1, Ordering::AcqRel);
            self.sync_starting.store(false, Ordering::Release);
        }
        #[cfg(target_os = "android")]
        crate::app::android_bg::set_periodic_sync(false);
        nova_sync::uninstall();
        if let Some(app) = self.app() {
            app.set_remote_has_playing_devices(false);
            app.set_remote_has_ready_devices(false);
            app.set_remote_devices(Default::default());
        }
        self.sync_status_to_ui();
    }
}

impl Bridge {
    /// Refresh the Settings → Sync readout from the engine (enable flag,
    /// identity, peers, status line).
    pub(super) fn sync_status_to_ui(&self) {
        addon_packages::reconcile(self);
        let Some(app) = self.app() else {
            return;
        };
        let settings = nova_sync::read_settings();
        app.set_persistence_failed(storage::last_error().is_some());
        app.set_sync_enabled(settings.enabled);
        app.set_sync_device_name(SharedString::from(&settings.device_name));
        app.set_sync_pair_confirm(settings.require_confirmation);
        app.set_sync_local_discovery(settings.enable_local_discovery);
        app.set_sync_interval_index(sync_interval_index(settings.interval_secs));
        app.set_sync_background(settings.background_enabled);
        match nova_sync::engine() {
            Some(engine) => {
                let names = engine.peer_names();
                let status = engine.status();
                let peers: Vec<SyncPeer> = engine
                    .peers()
                    .into_iter()
                    .map(|id| {
                        let name = names.get(&id).cloned().unwrap_or_default();
                        // Mesh-wide freshness: our direct ack or anyone's
                        // sighting, whichever is newer.
                        let mut last_seen = match engine.peer_last_seen(&id) {
                            Some(secs) => text::last_connected(&format_ago(secs)),
                            None => text::tr("Never connected").to_string(),
                        };
                        if let Some(attempt) = status.peer_attempts.get(&id) {
                            if attempt.in_flight {
                                last_seen = if attempt.last_error.as_deref()
                                    == Some("peer worker exceeded its attempt deadline")
                                {
                                    text::peer_retry(0)
                                } else {
                                    text::tr("Syncing…").to_string()
                                };
                            } else if attempt.last_error.is_some() {
                                let retry = attempt.retry_after_secs.saturating_sub(
                                    now_secs().saturating_sub(attempt.last_result_secs),
                                );
                                last_seen = text::peer_retry(retry);
                            }
                        }
                        SyncPeer {
                            id: id.into(),
                            name: name.into(),
                            last_seen: last_seen.into(),
                        }
                    })
                    .collect();
                let invites: Vec<SyncInvite> = engine
                    .invites()
                    .into_iter()
                    .map(|invite| SyncInvite {
                        id: invite.id.into(),
                    })
                    .collect();
                app.set_sync_identity(SharedString::from(engine.identity()));
                app.set_sync_peers(Rc::new(VecModel::from(peers)).into());
                app.set_sync_invites(Rc::new(VecModel::from(invites)).into());
                app.set_sync_status(SharedString::from(sync_status_with_packages(
                    &engine.status(),
                    addon_packages::sync_status(self),
                )));
            }
            None => {
                app.set_sync_identity(SharedString::default());
                app.set_sync_peers(Rc::new(VecModel::from(Vec::<SyncPeer>::new())).into());
                app.set_sync_invites(Rc::new(VecModel::from(Vec::<SyncInvite>::new())).into());
                app.set_sync_status(SharedString::default());
            }
        }
    }

    fn set_sync_notice(&self, notice: SyncNotice) {
        let rendered = notice.render();
        *self.account.sync_link_notice.lock().unwrap() = Some(notice);
        if let Some(app) = self.app() {
            app.set_sync_link_notice(SharedString::from(rendered));
        }
    }

    /// Re-render the latest one-off sync notice using the selected language.
    pub(super) fn refresh_sync_link_notice(&self) {
        let notice = self.account.sync_link_notice.lock().unwrap().clone();
        if let (Some(notice), Some(app)) = (notice, self.app()) {
            app.set_sync_link_notice(SharedString::from(notice.render()));
        }
    }

    /// Settings → Sync: the enable toggle flipped. Persists the flag and
    /// starts/stops the engine.
    pub(super) fn sync_set_enabled(&self, enabled: bool) {
        let mut settings = nova_sync::read_settings();
        settings.enabled = enabled;
        nova_sync::write_settings(&settings);
        if enabled {
            self.start_sync();
        } else {
            self.stop_sync();
        }
        self.sync_status_to_ui();
    }

    /// Settings → Sync: add a peer by endpoint id, then sync immediately.
    pub(super) fn sync_add_peer(&self, id: &str) {
        if let Some(app) = self.app() {
            app.set_sync_peer_input(SharedString::default());
        }
        if let Some(engine) = nova_sync::engine() {
            if let Err(e) = engine.add_peer(id) {
                crate::web_log(&format!("nova sync: {e}"));
            } else {
                engine.sync_now();
            }
        }
        self.sync_status_to_ui();
    }

    /// Settings → Sync: remove a peer by its endpoint id.
    pub(super) fn sync_remove_peer(&self, id: &str) {
        if let Some(engine) = nova_sync::engine() {
            engine.remove_peer(id);
        }
        self.sync_status_to_ui();
    }

    /// Settings → Sync: "Sync now".
    pub(super) fn sync_now(&self) {
        if let Some(engine) = nova_sync::engine() {
            addon_packages::retry_pending(self);
            engine.sync_now();
        }
        self.sync_status_to_ui();
    }

    /// Settings → Sync: copy this device's endpoint id (best effort).
    pub(super) fn sync_copy_identity(&self) {
        if let Some(engine) = nova_sync::engine() {
            copy_to_clipboard(&engine.identity());
        }
    }

    /// Pairing event from the engine (UI thread).
    pub(super) fn sync_pair_event(&self, event: nova_sync::PairEvent) {
        match event {
            nova_sync::PairEvent::Incoming(pair) => {
                if let Some(app) = self.app() {
                    app.set_sync_pair_prompt_id(SharedString::from(pair.id));
                    app.set_sync_pair_prompt_name(SharedString::from(pair.name));
                    app.set_sync_pair_prompt_code(SharedString::from(pair.code));
                    app.set_sync_pair_prompt_visible(true);
                }
            }
            nova_sync::PairEvent::Paired { name } => {
                if let Some(app) = self.app() {
                    app.set_sync_pair_prompt_visible(false);
                    app.set_sync_join_input(SharedString::default());
                    app.set_sync_invite_ticket(SharedString::default());
                    app.set_sync_invite_qr(Image::default());
                }
                self.set_sync_notice(SyncNotice::PairedWith(name));
                self.sync_status_to_ui();
                // Spread the new device to the rest of the mesh.
                fan_out();
            }
        }
    }

    /// Settings → Sync: accept or reject an incoming pairing request.
    pub(super) fn sync_respond_pair(&self, id: &str, accept: bool) {
        if let Some(engine) = nova_sync::engine() {
            engine.respond_pair(id, accept);
        }
        if let Some(app) = self.app() {
            app.set_sync_pair_prompt_visible(false);
        }
    }

    /// Settings → Sync: the device-name override changed.
    pub(super) fn sync_device_name_edited(&self, name: &str) {
        let mut settings = nova_sync::read_settings();
        settings.device_name = name.trim().to_string();
        nova_sync::write_settings(&settings);
    }

    /// Settings → Sync: the "ask before accepting" toggle flipped.
    pub(super) fn sync_pair_confirm_changed(&self, confirm: bool) {
        let mut settings = nova_sync::read_settings();
        settings.require_confirmation = confirm;
        nova_sync::write_settings(&settings);
    }

    /// Settings → Sync: the "local network discovery" toggle flipped.
    /// Endpoint builder options are fixed at bind time, so restart the
    /// engine for the change to take effect.
    pub(super) fn sync_local_discovery_changed(&self, enabled: bool) {
        let mut settings = nova_sync::read_settings();
        settings.enable_local_discovery = enabled;
        nova_sync::write_settings(&settings);
        if settings.enabled {
            // Invalidate setup in flight too: discovery is fixed at bind time.
            self.stop_sync();
            self.start_sync();
        }
        self.sync_status_to_ui();
    }

    /// Settings → Sync: the auto-sync interval preset picked. The background
    /// loop re-reads the interval every few seconds, so no restart is needed.
    pub(super) fn sync_interval_picked(&self, index: i32) {
        let mut settings = nova_sync::read_settings();
        settings.interval_secs = sync_interval_preset(index);
        nova_sync::write_settings(&settings);
        self.sync_status_to_ui();
    }

    /// Settings → Sync: the "background sync" toggle flipped. Schedules or
    /// cancels the Android periodic job; no engine restart needed.
    pub(super) fn sync_background_changed(&self, enabled: bool) {
        let mut settings = nova_sync::read_settings();
        settings.background_enabled = enabled;
        nova_sync::write_settings(&settings);
        #[cfg(target_os = "android")]
        crate::app::android_bg::set_periodic_sync(settings.enabled && settings.background_enabled);
        self.sync_status_to_ui();
    }

    /// Settings → Sync: create an invite and show its ticket.
    pub(super) fn sync_create_invite(&self) {
        let Some(engine) = nova_sync::engine() else {
            return;
        };
        match engine.create_invite() {
            Ok(ticket) => {
                if let Some(app) = self.app() {
                    app.set_sync_invite_qr(invite_qr_image(&ticket).unwrap_or_default());
                    app.set_sync_invite_ticket(SharedString::from(&ticket));
                }
                self.set_sync_notice(SyncNotice::Fixed(
                    "Invite ready — share the code; it expires in 15 minutes.",
                ));
            }
            Err(e) => {
                self.set_sync_notice(SyncNotice::CouldNotCreateInvite(e.to_string()));
            }
        }
        self.sync_status_to_ui();
    }

    /// Settings → Sync: revoke an outstanding invite by its short id.
    pub(super) fn sync_cancel_invite(&self, id: &str) {
        if let Some(engine) = nova_sync::engine() {
            engine.cancel_invite(id);
        }
        // The displayed ticket (if any) may be the one just revoked.
        if let Some(app) = self.app() {
            app.set_sync_invite_ticket(SharedString::default());
            app.set_sync_invite_qr(Image::default());
        }
        self.sync_status_to_ui();
    }

    /// Settings → Sync: copy an invite ticket.
    pub(super) fn sync_copy_ticket(&self, ticket: &str) {
        copy_to_clipboard(ticket);
    }

    /// Settings → Sync: join another device with an invite ticket.
    pub(super) fn sync_join_invite(&self, ticket: &str) {
        let Some(engine) = nova_sync::engine() else {
            return;
        };
        let result = engine.join_invite(ticket);
        if let Some(app) = self.app() {
            app.set_sync_join_input(SharedString::default());
            match result {
                Ok(()) => {
                    self.set_sync_notice(SyncNotice::Fixed("Connecting to the other device…"))
                }
                Err(e) => self.set_sync_notice(SyncNotice::InvalidInvite(e.to_string())),
            }
        }
    }

    /// Settings → Sync: open the camera QR scanner (Android only). The scanner
    /// decodes a ticket and calls [`Self::sync_join_invite`] itself, so this
    /// only launches the activity. A no-op on desktop, where the button is
    /// hidden by `sync_qr_scan_available`.
    pub(super) fn sync_scan_qr(&self) {
        #[cfg(target_os = "android")]
        crate::app::android_qr::start_scan();
        #[cfg(not(target_os = "android"))]
        {
            self.set_sync_notice(SyncNotice::Fixed(
                "Camera scanning is available on Android only.",
            ));
        }
    }
}

/// Fill the device name once (auto-detected) if the user hasn't set one.
pub(crate) fn ensure_device_name() {
    let mut settings = nova_sync::read_settings();
    if settings.device_name.trim().is_empty() {
        settings.device_name = detect_device_name();
        nova_sync::write_settings(&settings);
    }
}

/// Best-effort device name: Android model, else the desktop hostname.
fn detect_device_name() -> String {
    #[cfg(target_os = "android")]
    {
        let model = crate::player::device_model();
        if !model.trim().is_empty() {
            return model.trim().to_string();
        }
        return text::tr("Android device").to_string();
    }
    #[cfg(not(target_os = "android"))]
    {
        std::env::var("HOSTNAME")
            .ok()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Desktop".to_string())
    }
}

/// Timestamp of the last mesh fan-out, for rate limiting.
static LAST_FANOUT: AtomicU64 = AtomicU64::new(0);

/// Ask the engine to sync with all peers, at most once every few seconds, so
/// a newly introduced peer is connected to promptly without sync storms.
fn fan_out() {
    let now = now_secs();
    if now.saturating_sub(LAST_FANOUT.load(Ordering::Relaxed)) < 5 {
        return;
    }
    LAST_FANOUT.store(now, Ordering::Relaxed);
    if let Some(engine) = nova_sync::engine() {
        engine.sync_now();
    }
}

/// Auto-sync interval presets (seconds) for Settings → Sync, used while the
/// app is running. The default 60 s matches `SyncSettings::default`. The
/// closed-app cadence is the fixed JobScheduler job, not this setting.
pub(crate) const SYNC_INTERVAL_PRESETS: &[u64] = &[30, 60, 300, 900];

/// Preset seconds for a segmented-control index (clamped into range).
pub(crate) fn sync_interval_preset(index: i32) -> u64 {
    SYNC_INTERVAL_PRESETS
        .get(index.clamp(0, SYNC_INTERVAL_PRESETS.len() as i32 - 1) as usize)
        .copied()
        .unwrap_or(60)
}

/// Segmented-control index for stored seconds: the nearest preset, so
/// hand-edited values still display sensibly. Ties round up.
pub(crate) fn sync_interval_index(secs: u64) -> i32 {
    let mut best = 0;
    for (i, preset) in SYNC_INTERVAL_PRESETS.iter().enumerate() {
        if secs >= *preset {
            best = i;
        } else {
            // Past the midpoint between `best` and this preset: round up.
            if secs * 2 >= SYNC_INTERVAL_PRESETS[best] + preset {
                best = i;
            }
            break;
        }
    }
    best as i32
}

/// Human-readable status line for Settings → Sync.
fn sync_status_text(status: &nova_sync::SyncStatus) -> String {
    if status.syncing {
        return text::tr("Syncing…").to_string();
    }
    if let Some(err) = &status.last_error {
        return text::last_sync_failed(err);
    }
    if status.last_sync_secs == 0 {
        return if status.peer_count == 0 {
            text::tr("Add a peer to start syncing").to_string()
        } else {
            text::tr("Not synced yet").to_string()
        };
    }
    text::last_synced(&format_ago(status.last_sync_secs))
}

fn sync_status_with_packages(
    status: &nova_sync::SyncStatus,
    packages: addon_packages::PackageSyncStatus,
) -> String {
    if packages.pending == 0 && packages.sending == 0 {
        return sync_status_text(status);
    }
    let mut lines = Vec::new();
    // Retain record errors and active record work. A historical success must
    // not imply that the requested addon code has already been installed.
    if status.syncing || status.last_error.is_some() {
        lines.push(sync_status_text(status));
    } else if status.peer_count == 0 {
        lines.push(text::tr("Add a peer to start syncing").to_string());
    }
    if packages.pending > 0 {
        lines.push(if packages.transferring > 0 {
            text::syncing_addons(packages.pending)
        } else if let Some(secs) = packages.retry_secs {
            text::addon_sync_retry(packages.pending, secs)
        } else {
            text::addon_sync_waiting(packages.pending)
        });
    }
    if packages.sending > 0 {
        lines.push(text::sending_addons(packages.sending));
    }
    lines.join("\n")
}

/// Relative age ("5s ago", "3m ago", "2h ago") of a past unix-secs
/// timestamp. Shared by the status line and the per-peer last-seen labels.
pub(crate) fn format_ago(secs: u64) -> String {
    text::ago(now_secs().saturating_sub(secs))
}

fn activity_records(
    activity: &HashMap<String, ViewingActivity>,
) -> Result<Vec<(String, String, u64)>, serde_json::Error> {
    activity
        .iter()
        .map(|(id, value)| Ok((id.clone(), serde_json::to_string(value)?, 0)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactive_profiles_materialize_without_changing_local_selection() {
        const ROOT: &str = "NOVA_SYNC_PROFILE_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-sync-profiles-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .args(["--exact", "app::sync::tests::inactive_profiles_materialize_without_changing_local_selection", "--nocapture"])
                .output().unwrap();
            let _ = fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        nova_storage::init_at(&PathBuf::from(root));
        let active = profiles::current_id();
        let active_key = profiles::storage_key_for(&active, "library");
        nova_storage::try_set_str(&active_key, "[]").unwrap();
        let bob_library = nova_sync::profile_domain("bob", DOMAIN_LIBRARY);
        let bob_settings = nova_sync::profile_domain("bob", DOMAIN_SETTINGS);
        let owner = nova_sync::local_store().unwrap();
        {
            let mut store = owner.lock().unwrap();
            store.set(
                &bob_library,
                "show",
                Some(serde_json::to_string(&entry("show", 1)).unwrap()),
                0,
                9,
            );
            store.set(
                &bob_settings,
                "language",
                Some(serde_json::to_string(&nova_config::Language::Croatian).unwrap()),
                0,
                9,
            );
            store.save().unwrap();
        }
        let before = owner.lock().unwrap().digest();
        project_inactive_profile("bob", &bob_library, DOMAIN_LIBRARY).unwrap();
        project_inactive_profile("bob", &bob_settings, DOMAIN_SETTINGS).unwrap();
        assert_eq!(profiles::current_id(), active);
        assert_eq!(
            nova_storage::try_get_str(&active_key).unwrap().as_deref(),
            Some("[]")
        );
        let bob_raw = nova_storage::try_get_str(&profiles::storage_key_for("bob", "library"))
            .unwrap()
            .unwrap();
        let bob_entries: Vec<LibraryEntry> = serde_json::from_str(&bob_raw).unwrap();
        assert_eq!(bob_entries[0].id, "show");
        let settings: CacheSettings = serde_json::from_str(
            &nova_storage::try_get_str(&profiles::storage_key_for("bob", "settings"))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(settings.language, nova_config::Language::Croatian);
        assert_eq!(
            owner.lock().unwrap().digest(),
            before,
            "projection cannot create a new action"
        );
        {
            let mut store = owner.lock().unwrap();
            store.set("profile_deletions", "bob", Some("1".into()), 0, 9);
            store.apply(
                &bob_library,
                "stale",
                nova_sync::Record {
                    value: Some(serde_json::to_string(&entry("stale", 2)).unwrap()),
                    version: nova_sync::Version::new(nova_config::now_ms(), 0, 9, false),
                },
            );
            store.save().unwrap();
        }
        let mut stale_peer = nova_sync::Store::default();
        {
            let store = owner.lock().unwrap();
            for outbound in store.outbound(&std::collections::BTreeMap::new()) {
                stale_peer.apply(&outbound.domain, &outbound.key, outbound.record);
            }
        }
        project_inactive_profile("bob", &bob_library, DOMAIN_LIBRARY).unwrap();
        let retired_digest = {
            let store = owner.lock().unwrap();
            assert!(store.records(&bob_library).is_empty());
            assert!(store.record(&bob_library, "stale").unwrap().is_deleted());
            assert!(!store.pending_domains().contains(&bob_library));
            for outbound in store.outbound(&stale_peer.digest()) {
                stale_peer.apply(&outbound.domain, &outbound.key, outbound.record);
            }
            store.digest()
        };
        assert!(stale_peer.records(&bob_library).is_empty());
        assert_eq!(stale_peer.digest(), retired_digest);
        project_inactive_profile("bob", &bob_library, DOMAIN_LIBRARY).unwrap();
        assert_eq!(
            owner.lock().unwrap().digest(),
            retired_digest,
            "replaying retirement must not generate more actions"
        );
        assert_eq!(
            nova_storage::try_get_str(&profiles::storage_key_for("bob", "library"))
                .unwrap()
                .as_deref(),
            Some(bob_raw.as_str())
        );
        assert_eq!(profiles::current_id(), active);
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn synced_episode_aliases_agree_on_completion_and_explicit_unwatch() {
        const ROOT: &str = "NOVA_SYNC_ALIAS_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-sync-alias-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .args(["--exact", "app::sync::tests::synced_episode_aliases_agree_on_completion_and_explicit_unwatch", "--nocapture"])
                .output().unwrap();
            let _ = fs::remove_dir_all(root);
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
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let (hi, _) = mpsc::channel();
        let (lo, _) = mpsc::channel();
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        let series = "tt26743760";
        let native = "kitsu:50021:2";
        let canonical = "tt26743760:3:2";
        let updated = now_secs();
        let videos = |id: &str| {
            serde_json::from_value::<Vec<Video>>(serde_json::json!([
            {"id":id, "season":3, "episode":2, "name":"The Fire-Rat Cloak", "released":"2000-01-01",
             "novaMetadataRevision":nova_providers::metadata_revision(), "novaStreamIds":[native, canonical]},
            {"id":"tt26743760:3:3", "season":3, "episode":3, "released":"2999-01-01"}
        ])).unwrap()
        };
        write_episodes_cache_for("series", series, &videos(native));
        let mut source = nova_sync::Store::default();
        for (episode, position, watched, dev) in
            [(native, 1150.0, false, 1), (canonical, 0.0, true, 2)]
        {
            let p = EpisodeProgress {
                series_id: series.into(),
                episode_id: episode.into(),
                position_secs: position,
                duration_secs: 1440.0,
                watched,
                updated_at_secs: updated,
                ..Default::default()
            };
            source.set(
                &profile_domain(DOMAIN_PROGRESS),
                &progress_map_key(series, episode),
                Some(serde_json::to_string(&p).unwrap()),
                0,
                dev,
            );
        }
        let mut saved = entry(series, 1);
        saved.type_ = "series".into();
        source.set(
            &profile_domain(DOMAIN_LIBRARY),
            series,
            Some(serde_json::to_string(&saved).unwrap()),
            0,
            2,
        );
        let owner = nova_sync::local_store().unwrap();
        {
            let mut target = owner.lock().unwrap();
            for (episode, _) in source.records(&profile_domain(DOMAIN_PROGRESS)) {
                target.apply(
                    &profile_domain(DOMAIN_PROGRESS),
                    &episode,
                    source
                        .record(&profile_domain(DOMAIN_PROGRESS), &episode)
                        .unwrap()
                        .clone(),
                );
            }
            target.apply(
                &profile_domain(DOMAIN_LIBRARY),
                series,
                source
                    .record(&profile_domain(DOMAIN_LIBRARY), series)
                    .unwrap()
                    .clone(),
            );
            target.save().unwrap();
        }
        let digest = owner.lock().unwrap().digest();
        bridge.sync_apply(vec![DOMAIN_LIBRARY.into(), DOMAIN_PROGRESS.into()]);
        assert!(
            bridge.shared.lock().unwrap().progress[&progress_map_key(series, native)].watched,
            "the native episode must honor completion under its confirmed canonical alias"
        );
        assert_eq!(app.get_home_continue().row_count(), 0);
        assert_eq!(app.get_home_upcoming().row_count(), 1);
        bridge.open_library_item(0);
        assert!(app.get_episode_rows().row_data(0).unwrap().watched);
        assert_eq!(
            app.get_episode_rows().row_data(0).unwrap().progress,
            0.0,
            "manual completion at zero position keeps its checkmark without a fake playback rail"
        );
        assert_eq!(
            owner.lock().unwrap().digest(),
            digest,
            "view reconciliation must not invent sync actions"
        );
        let worker = persistence::start().unwrap();
        let native_record = owner
            .lock()
            .unwrap()
            .record(
                &profile_domain(DOMAIN_PROGRESS),
                &progress_map_key(series, native),
            )
            .unwrap()
            .clone();
        let map = {
            let mut state = bridge.shared.lock().unwrap();
            state.progress.insert(
                progress_map_key("other", "other:1:1"),
                EpisodeProgress {
                    series_id: "other".into(),
                    episode_id: "other:1:1".into(),
                    position_secs: 100.0,
                    duration_secs: 1000.0,
                    updated_at_secs: now_secs(),
                    ..Default::default()
                },
            );
            state.progress.clone()
        };
        write_progress_map(&map);
        persistence::flush().unwrap();
        assert_eq!(
            owner
                .lock()
                .unwrap()
                .record(
                    &profile_domain(DOMAIN_PROGRESS),
                    &progress_map_key(series, native)
                )
                .unwrap(),
            &native_record,
            "an unrelated save must not turn an alias view into a new watch action"
        );
        {
            let mut state = bridge.shared.lock().unwrap();
            Bridge::set_episode_watched_locked(&mut state.progress, series, native, false);
            let p = state
                .progress
                .get_mut(&progress_map_key(series, native))
                .unwrap();
            p.updated_at_secs = updated;
            p.unwatched_at_secs = updated;
        }
        bridge.persist_and_refresh_progress();
        bridge.sync_apply(vec![DOMAIN_PROGRESS.into()]);
        assert!(!app.get_episode_rows().row_data(0).unwrap().watched);
        assert_eq!(app.get_episode_rows().row_data(0).unwrap().progress, 0.0);
        assert_eq!(app.get_home_upcoming().row_count(), 0);
        let after_unwatch = owner.lock().unwrap().digest();
        // Another device can display the canonical ID from a cold cache.
        // Completion/unwatch agree in either representation, even within the
        // same Unix second; action HLCs, not timestamp heuristics, decide.
        let canonical_videos = videos(canonical);
        write_episodes_cache_for("series", series, &canonical_videos);
        bridge.show_episode_picker(series.into(), canonical_videos);
        assert!(!app.get_episode_rows().row_data(0).unwrap().watched);
        assert_eq!(owner.lock().unwrap().digest(), after_unwatch);
        {
            let mut state = bridge.shared.lock().unwrap();
            Bridge::set_episode_watched_locked(&mut state.progress, series, canonical, true);
            state
                .progress
                .get_mut(&progress_map_key(series, canonical))
                .unwrap()
                .updated_at_secs = updated;
        }
        bridge.persist_and_refresh_progress();
        bridge.sync_apply(vec![DOMAIN_PROGRESS.into()]);
        assert!(app.get_episode_rows().row_data(0).unwrap().watched);
        let after_rewatch = owner.lock().unwrap().digest();
        let native_videos = videos(native);
        write_episodes_cache_for("series", series, &native_videos);
        bridge.show_episode_picker(series.into(), native_videos);
        assert!(app.get_episode_rows().row_data(0).unwrap().watched);
        assert_eq!(app.get_home_continue().row_count(), 0);
        assert_eq!(app.get_home_upcoming().row_count(), 1);
        assert_eq!(owner.lock().unwrap().digest(), after_rewatch);
        assert!(
            !bridge.sync_reconcile_progress_aliases(),
            "repeated reconciliation is idle"
        );
        drop(worker);
        let restored = nova_sync::Store::try_load().unwrap();
        assert_eq!(restored.digest(), after_rewatch);
        assert!(read_progress_map()[&progress_map_key(series, native)].watched);
    }

    #[test]
    fn progress_aliases_reject_stale_ambiguous_and_number_only_links() {
        let video = |id: &str, alias: Option<&str>, revision: u64| {
            let mut v = Video {
                id: id.into(),
                season: Some(3),
                episode: Some(2),
                ..Default::default()
            };
            if let Some(alias) = alias {
                v.extra
                    .insert("novaMetadataRevision".into(), serde_json::json!(revision));
                v.extra
                    .insert("novaStreamIds".into(), serde_json::json!([alias]));
            }
            v
        };
        let revision = nova_providers::metadata_revision();
        assert!(
            episode_progress_alias_groups(&[
                video("a", None, revision),
                video("b", None, revision)
            ])
            .is_empty()
        );
        assert!(
            episode_progress_alias_groups(&[video("a", Some("tt1:3:2"), revision.wrapping_add(1))])
                .is_empty()
        );
        assert!(
            episode_progress_alias_groups(&[
                video("a", Some("tt1:3:2"), revision),
                video("b", Some("tt1:3:2"), revision)
            ])
            .is_empty()
        );
        assert_eq!(
            episode_progress_alias_groups(&[video("a", Some("tt1:3:2"), revision)]),
            vec![("a".into(), vec!["a".into(), "tt1:3:2".into()])]
        );
    }

    #[test]
    fn projection_origin_is_nested_and_thread_local() {
        let outer = ApplyingGuard::new();
        {
            let _inner = ApplyingGuard::new();
            assert!(applying());
        }
        assert!(applying());
        assert!(!std::thread::spawn(applying).join().unwrap());
        drop(outer);
        assert!(!applying());
    }

    fn entry(id: &str, added_at_secs: u64) -> LibraryEntry {
        LibraryEntry {
            id: id.into(),
            type_: "movie".into(),
            name: id.into(),
            year: String::new(),
            poster_url: String::new(),
            background_url: String::new(),
            genres: Vec::new(),
            description: String::new(),
            categories: Vec::new(),
            watch_status: WatchStatus::Auto,
            imported_tracking_status: None,
            added_at_secs,
        }
    }

    #[test]
    fn backfill_assigns_saved_order_once() {
        let mut entries = vec![entry("a", 0), entry("b", 0), entry("c", 5)];
        assert!(backfill_added_at(&mut entries));
        assert_eq!(entries[0].added_at_secs, 1);
        assert_eq!(entries[1].added_at_secs, 2);
        assert_eq!(entries[2].added_at_secs, 5);
        // Idempotent: a second pass changes nothing.
        assert!(!backfill_added_at(&mut entries));
    }

    fn stores(urls: &[&str]) -> Vec<AddonStore> {
        urls.iter()
            .map(|u| AddonStore {
                capabilities: None,
                url: u.to_string(),
                enabled: true,
                configure_ok: None,
                label: u.to_string(),
            })
            .collect()
    }

    #[test]
    fn addon_order_record_round_trips() {
        let addons = stores(&["https://b.example", "https://a.example"]);
        let (key, json, ts) = addon_order_record(&addons);
        assert_eq!(key, ADDON_ORDER_KEY);
        assert_eq!(ts, 0);
        let back: Vec<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, vec!["https://b.example", "https://a.example"]);
    }

    #[test]
    fn sort_by_order_lists_known_first_and_appends_unknown() {
        let current = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        // Reversed mesh order wins wholesale.
        assert_eq!(
            sort_by_order(current.clone(), &["c".to_string(), "a".to_string()]),
            vec!["c".to_string(), "a".to_string(), "b".to_string()]
        );
        // Unknown urls keep trailing relative order; stale entries drop out.
        assert_eq!(
            sort_by_order(current.clone(), &["c".to_string(), "z".to_string()]),
            vec!["c".to_string(), "a".to_string(), "b".to_string()]
        );
        // Empty order keeps everything as-is.
        assert_eq!(sort_by_order(current.clone(), &Vec::new()), current);
    }

    #[test]
    fn interval_preset_round_trips_and_clamps() {
        assert_eq!(SYNC_INTERVAL_PRESETS.len(), 4);
        for (i, secs) in SYNC_INTERVAL_PRESETS.iter().enumerate() {
            assert_eq!(sync_interval_preset(i as i32), *secs);
            assert_eq!(sync_interval_index(*secs), i as i32);
        }
        // Out-of-range indices clamp instead of panicking.
        assert_eq!(sync_interval_preset(-1), 30);
        assert_eq!(sync_interval_preset(99), 900);
        // In-between and out-of-range values snap to the nearest preset.
        assert_eq!(sync_interval_index(0), 0);
        assert_eq!(sync_interval_index(61), 1);
        assert_eq!(sync_interval_index(200), 2);
        assert_eq!(sync_interval_index(600), 3);
        assert_eq!(sync_interval_index(10_000), 3);
    }

    #[test]
    fn progress_values_ignore_unknown_fields() {
        // A newer peer may add fields to the record; older builds must still
        // read it instead of dropping the episode's progress.
        let p: EpisodeProgress = serde_json::from_str(
            r#"{"series_id":"s","episode_id":"e","position_secs":12.5,"duration_secs":600.0,"watched":false,"play_count":3,"updated_at_secs":99,"future_field":true}"#,
        )
        .unwrap();
        assert_eq!(p.position_secs, 12.5);
        assert_eq!(p.play_count, 3);
        assert_eq!(p.updated_at_secs, 99);
    }

    fn prog(
        pos: f64,
        dur: f64,
        watched: bool,
        unwatched_at: u64,
        plays: u32,
        at: u64,
    ) -> EpisodeProgress {
        EpisodeProgress {
            series_id: "s".into(),
            episode_id: "e".into(),
            position_secs: pos,
            duration_secs: dur,
            watched,
            unwatched_at_secs: unwatched_at,
            play_count: plays,
            updated_at_secs: at,
            history: None,
        }
    }

    #[test]
    fn merge_stale_unwatched_never_clobbers_watched() {
        // The regression: B watched to 30% (newer) while A had finished.
        let local = prog(1800.0, 1800.0, true, 0, 2, 100);
        let remote = prog(900.0, 1800.0, false, 0, 1, 110);
        let merged = merge_progress(Some(&local), &remote);
        assert!(merged.watched);
        assert_eq!(merged.position_secs, 1800.0);
        assert_eq!(merged.play_count, 2);
        assert_eq!(merged.updated_at_secs, 110);
        // Symmetric: watched on the remote side wins too.
        let merged = merge_progress(Some(&remote), &local);
        assert!(merged.watched);
        assert_eq!(merged.position_secs, 1800.0);
    }

    #[test]
    fn merge_explicit_unwatch_wins_and_rewatch_recovers() {
        // Explicit unwatch (zeroed position + fresh intent) beats stale watched.
        let watched = prog(1800.0, 1800.0, true, 0, 2, 100);
        let unwatch = prog(0.0, 1800.0, false, 110, 2, 110);
        let merged = merge_progress(Some(&watched), &unwatch);
        assert!(!merged.watched);
        assert_eq!(merged.position_secs, 0.0);
        // Watching again clears the intent: the rewatch wins next time.
        let rewatch = prog(1800.0, 1800.0, true, 0, 3, 120);
        let merged = merge_progress(Some(&unwatch), &rewatch);
        assert!(merged.watched);
        assert_eq!(merged.play_count, 3);
        // And the stale unwatch never resurrects against the rewatch.
        let merged = merge_progress(Some(&rewatch), &unwatch);
        assert!(merged.watched);
    }

    #[test]
    fn merge_both_unwatched_is_recency() {
        // Deliberate rewinds survive: newer (lower) position wins.
        let older = prog(900.0, 1800.0, false, 0, 1, 100);
        let newer = prog(120.0, 1800.0, false, 0, 1, 110);
        let merged = merge_progress(Some(&older), &newer);
        assert!(!merged.watched);
        assert_eq!(merged.position_secs, 120.0);
        // A cleared resume (fresh zero, no unwatch intent) wins as an update.
        let cleared = prog(0.0, 1800.0, false, 0, 1, 120);
        let merged = merge_progress(Some(&newer), &cleared);
        assert_eq!(merged.position_secs, 0.0);
    }

    #[test]
    fn merge_none_local_passes_through() {
        let remote = prog(10.0, 100.0, false, 0, 1, 50);
        assert_eq!(
            serde_json::to_string(&merge_progress(None, &remote)).unwrap(),
            serde_json::to_string(&remote).unwrap()
        );
    }

    #[test]
    fn format_ago_buckets() {
        // The language is process-wide: run under the same lock the `text`
        // tests take so a concurrent Croatian test cannot shift these strings.
        crate::app::text::with_language(Language::English, || {
            let now = now_secs();
            assert_eq!(format_ago(now), "0s ago");
            assert_eq!(format_ago(now.saturating_sub(45)), "45s ago");
            assert_eq!(format_ago(now.saturating_sub(180)), "3m ago");
            assert_eq!(format_ago(now.saturating_sub(7200)), "2h ago");
            // Future timestamps (clock skew) clamp to zero, never underflow.
            assert_eq!(format_ago(now + 1000), "0s ago");
        });
    }

    #[test]
    fn status_text_reports_states() {
        crate::app::text::with_language(Language::English, || {
            let mut status = nova_sync::SyncStatus::default();
            assert_eq!(sync_status_text(&status), "Add a peer to start syncing");
            status.peer_count = 2;
            assert_eq!(sync_status_text(&status), "Not synced yet");
            status.syncing = true;
            assert_eq!(sync_status_text(&status), "Syncing…");
            status.syncing = false;
            status.last_error = Some("boom".into());
            assert_eq!(sync_status_text(&status), "Last sync failed: boom");
        });
    }

    #[test]
    fn sync_completion_includes_addon_transfers_validation_and_retry_states() {
        use addon_packages::PackageSyncStatus;
        crate::app::text::with_language(Language::English, || {
            let mut records = nova_sync::SyncStatus {
                peer_count: 1,
                last_sync_secs: now_secs(),
                ..Default::default()
            };
            let mut packages = PackageSyncStatus {
                pending: 1,
                transferring: 1,
                ..Default::default()
            };
            assert_eq!(
                sync_status_with_packages(&records, packages),
                "Syncing addons… (1 remaining)"
            );
            records.syncing = true;
            assert_eq!(
                sync_status_with_packages(&records, packages),
                "Syncing…\nSyncing addons… (1 remaining)"
            );
            records.syncing = false;
            packages.transferring = 0;
            packages.retry_secs = Some(20);
            assert_eq!(
                sync_status_with_packages(&records, packages),
                "Addon sync incomplete: 1 pending · retrying in 20s"
            );
            packages.retry_secs = None;
            assert_eq!(
                sync_status_with_packages(&records, packages),
                "Sync incomplete: waiting for addons (1)"
            );
            records.last_error = Some("offline".into());
            assert_eq!(
                sync_status_with_packages(&records, packages),
                "Last sync failed: offline\nSync incomplete: waiting for addons (1)"
            );
            records.last_error = None;
            records.peer_count = 0;
            assert_eq!(
                sync_status_with_packages(&records, packages),
                "Add a peer to start syncing\nSync incomplete: waiting for addons (1)"
            );
            records.peer_count = 1;
            packages = PackageSyncStatus {
                sending: 1,
                ..Default::default()
            };
            assert_eq!(
                sync_status_with_packages(&records, packages),
                "Sending addons… (1)"
            );
            assert!(
                sync_status_with_packages(&records, PackageSyncStatus::default())
                    .starts_with("Last synced ")
            );
        });
        crate::app::text::with_language(Language::Croatian, || {
            let records = nova_sync::SyncStatus {
                peer_count: 1,
                ..Default::default()
            };
            assert_eq!(
                sync_status_with_packages(
                    &records,
                    PackageSyncStatus {
                        pending: 2,
                        transferring: 1,
                        ..Default::default()
                    }
                ),
                "Sinkronizacija dodataka… (preostalo: 2)"
            );
        });
    }

    #[test]
    fn tmdb_recommendation_key_syncs_and_preserves_unrelated_settings() {
        assert!(setting_can_override("tmdb_api_key"));
        let local = CacheSettings {
            tmdb_api_key: "0123456789abcdef0123456789abcdef".into(),
            ..Default::default()
        };
        let fields = settings_fields(&local);
        let key = fields
            .iter()
            .find(|(field, _, _)| field == "tmdb_api_key")
            .unwrap();
        let merged =
            merge_settings_fields(&CacheSettings::default(), &[(key.0.clone(), key.1.clone())]);
        assert_eq!(merged.tmdb_api_key, local.tmdb_api_key);
        assert_eq!(merged.language, Language::English);
        let cleared = merge_settings_fields(&merged, &[("tmdb_api_key".into(), "\"\"".into())]);
        assert!(cleared.tmdb_api_key.is_empty());
    }

    #[test]
    fn automatic_next_episode_syncs_independently_and_supports_override() {
        let local = CacheSettings {
            auto_next_episode: true,
            ..Default::default()
        };
        let fields: HashMap<_, _> = settings_fields(&local)
            .into_iter()
            .map(|(key, value, _)| (key, value))
            .collect();
        assert_eq!(
            fields.get("auto_next_episode").map(String::as_str),
            Some("true")
        );
        assert!(setting_can_override("auto_next_episode"));
        let merged = merge_settings_fields(&local, &[("auto_next_episode".into(), "false".into())]);
        assert!(!merged.auto_next_episode);
        assert!(merged.prefetch_next_episode_streams);
    }

    #[test]
    fn automatic_intro_skip_is_independent_and_can_be_overridden() {
        let settings = CacheSettings {
            intro_skip_mode: nova_config::IntroSkipMode::Auto,
            intro_skip_confidence: nova_config::IntroSkipConfidence::High,
            ..Default::default()
        };
        let fields: HashMap<_, _> = settings_fields(&settings)
            .into_iter()
            .map(|(key, value, _)| (key, value))
            .collect();
        assert_eq!(
            fields.get("intro_skip_mode").map(String::as_str),
            Some("\"auto\"")
        );
        assert!(setting_can_override("intro_skip_mode"));
        assert!(setting_can_override("intro_skip_confidence"));
        assert_eq!(
            fields.get("intro_skip_confidence").map(String::as_str),
            Some("\"high\"")
        );
        let merged = merge_settings_fields(
            &settings,
            &[("intro_skip_confidence".into(), "\"low\"".into())],
        );
        assert_eq!(
            merged.intro_skip_confidence,
            nova_config::IntroSkipConfidence::Low
        );
        assert_eq!(merged.intro_skip_mode, nova_config::IntroSkipMode::Auto);
        assert_eq!(
            CacheSettings::default().intro_skip_mode,
            nova_config::IntroSkipMode::Prompt
        );
    }

    #[test]
    fn next_episode_prefetch_syncs_and_can_be_overridden() {
        let settings = CacheSettings {
            prefetch_next_episode_streams: false,
            ..Default::default()
        };
        let fields: HashMap<_, _> = settings_fields(&settings)
            .into_iter()
            .map(|(key, value, _)| (key, value))
            .collect();
        assert_eq!(
            fields
                .get("prefetch_next_episode_streams")
                .map(String::as_str),
            Some("false")
        );
        assert!(setting_can_override("prefetch_next_episode_streams"));
    }

    #[test]
    fn chapter_display_syncs_and_can_be_overridden() {
        let local = CacheSettings::default();
        assert!(local.show_chapters);
        assert!(setting_can_override("show_chapters"));
        let fields: HashMap<_, _> = settings_fields(&local)
            .into_iter()
            .map(|(key, value, _)| (key, value))
            .collect();
        assert_eq!(
            fields.get("show_chapters").map(String::as_str),
            Some("true")
        );
        let merged = merge_settings_fields(&local, &[("show_chapters".into(), "false".into())]);
        assert!(!merged.show_chapters);
        assert_eq!(merged.intro_skip_mode, local.intro_skip_mode);
    }

    #[test]
    fn episode_start_behavior_syncs_as_an_independent_setting() {
        let local = CacheSettings {
            episode_start_behavior: nova_config::EpisodeStartBehavior::Ask,
            ..Default::default()
        };
        let fields: HashMap<_, _> = settings_fields(&local)
            .into_iter()
            .map(|(key, value, _)| (key, value))
            .collect();
        assert_eq!(
            fields.get("episode_start_behavior").map(String::as_str),
            Some("\"Ask\"")
        );
        let merged = merge_settings_fields(
            &local,
            &[("episode_start_behavior".into(), "\"StartOver\"".into())],
        );
        assert_eq!(
            merged.episode_start_behavior,
            nova_config::EpisodeStartBehavior::StartOver
        );
    }

    #[test]
    fn settings_fields_split_per_field_and_group_cache() {
        let settings = CacheSettings {
            image_disk_cache_limit_mb: Some(2048),
            quality: 91,
            detail_poster_max_side: Some(720),
            detail_backdrop_max_side: None,
            episode_thumbnail_max_side: Some(360),
            player_episode_backdrop_max_side: None,
            animations: false,
            categories: vec!["Anime".into()],
            android_hwdec: AndroidHwdec::Sw,
            player_external: true,
            desktop_external_app: DesktopExternalApp::Vlc,
            playback_speed: 1.5,
            home_catalog_sources: vec![HomeCatalogSource {
                addon_url: "https://example.test/manifest.json".into(),
                type_: "series".into(),
                catalog_id: "trending".into(),
                genre: "Action".into(),
            }],
            ..Default::default()
        };

        let fields: HashMap<String, String> = settings_fields(&settings)
            .into_iter()
            .map(|(name, json, _)| (name, json))
            .collect();
        // Independent settings sync as their own records.
        assert_eq!(fields.get("animations").map(String::as_str), Some("false"));
        assert_eq!(fields["image_disk_cache_limit_mb"], "2048");
        // Cache settings are coupled into one whole-record blob.
        let cache: serde_json::Value =
            serde_json::from_str(fields.get("cache").expect("cache group")).unwrap();
        assert_eq!(cache["quality"], 91);
        assert!(cache.get("image_disk_cache_limit_mb").is_none());
        let unlimited = merge_settings_fields(
            &settings,
            &[("image_disk_cache_limit_mb".into(), "null".into())],
        );
        assert_eq!(unlimited.image_disk_cache_limit_mb, None);
        let mut local = settings.clone();
        local
            .sync_overrides
            .insert("image_disk_cache_limit_mb".into(), serde_json::json!(2048));
        let merged = merge_settings_fields(
            &local,
            &[("image_disk_cache_limit_mb".into(), "4096".into())],
        );
        assert_eq!(merged.image_disk_cache_limit_mb, Some(2048));
        assert_eq!(
            merged.sync_overrides["image_disk_cache_limit_mb"],
            serde_json::json!(4096)
        );
        assert_eq!(cache["cache_images"], true);
        assert_eq!(cache["detail_poster_max_side"], 720);
        assert_eq!(cache["detail_backdrop_max_side"], serde_json::Value::Null);
        assert_eq!(cache["episode_thumbnail_max_side"], 360);
        assert_eq!(
            cache["player_episode_backdrop_max_side"],
            serde_json::Value::Null
        );
        assert!(!fields.contains_key("quality"));
        // Categories use their own union domain; player and display choices
        // below are device-specific.
        assert!(!fields.contains_key("categories"));
        assert!(!fields.contains_key("android_hwdec"));
        assert!(!fields.contains_key("desktop_hwdec"));
        assert!(!fields.contains_key("player_external"));
        assert!(!fields.contains_key("remote_control_enabled"));
        assert_eq!(fields["completion_rating_prompts_enabled"], "true");
        assert!(
            !merge_settings_fields(
                &settings,
                &[("completion_rating_prompts_enabled".into(), "false".into())]
            )
            .completion_rating_prompts_enabled
        );
        assert!(!fields.contains_key("desktop_external_app"));
        assert!(fields.contains_key("playback_speed"));
        assert!(fields.contains_key("subtitle_style"));
        assert!(setting_can_override("subtitle_style"));
        let style = settings.subtitle_style.with_value(2, 90.0);
        let style_records = [(
            "subtitle_style".into(),
            serde_json::to_string(&style).unwrap(),
        )];
        assert_eq!(
            merge_settings_fields(&settings, &style_records)
                .subtitle_style
                .margin,
            90.0
        );
        let mut local_style = settings.clone();
        local_style.sync_overrides.insert(
            "subtitle_style".into(),
            serde_json::to_value(style).unwrap(),
        );
        assert_eq!(
            merge_settings_fields(&local_style, &style_records).subtitle_style,
            settings.subtitle_style
        );
        assert!(fields.contains_key("default_audio_language"));
        assert!(fields.contains_key("default_subtitle_language"));
        assert!(!fields.contains_key("android_auto_continue"));
        assert!(!fields.contains_key("android_pip_enabled"));
        let remote_pip = [("android_pip_enabled".into(), "false".into())];
        assert!(merge_settings_fields(&settings, &remote_pip).android_pip_enabled);
        assert_eq!(fields["background_style"], "\"default\"");
        let base = CacheSettings {
            background_style: nova_config::BackgroundStyle::TrueBlack,
            ..Default::default()
        };
        let legacy = [("true_black".into(), "false".into())];
        let merged = merge_settings_fields(&base, &legacy);
        assert_eq!(
            merged.background_style,
            nova_config::BackgroundStyle::Default
        );
        let remote = [("background_style".into(), "\"accent_tinted\"".into())];
        let merged = merge_settings_fields(&base, &remote);
        assert_eq!(
            merged.background_style,
            nova_config::BackgroundStyle::AccentTinted
        );
        for both in [
            vec![legacy[0].clone(), remote[0].clone()],
            vec![remote[0].clone(), legacy[0].clone()],
        ] {
            assert_eq!(
                merge_settings_fields(&base, &both).background_style,
                nova_config::BackgroundStyle::AccentTinted
            );
        }
        let mut overridden = base;
        overridden
            .sync_overrides
            .insert("background_style".into(), serde_json::json!("default"));
        let merged = merge_settings_fields(&overridden, &remote);
        assert_eq!(
            merged.background_style,
            nova_config::BackgroundStyle::TrueBlack
        );
        assert_eq!(
            merged.sync_overrides["background_style"],
            serde_json::json!("accent_tinted")
        );
        let published: HashMap<_, _> = settings_fields(&merged)
            .into_iter()
            .map(|(k, v, _)| (k, v))
            .collect();
        assert_eq!(published["background_style"], "\"accent_tinted\"");
        let home_sources: serde_json::Value = serde_json::from_str(
            fields
                .get("home_catalog_sources")
                .expect("Home catalog selections sync as one setting"),
        )
        .unwrap();
        assert_eq!(home_sources[0]["catalogId"], "trending");
        assert_eq!(home_sources[0]["genre"], "Action");
    }

    #[test]
    fn migrated_home_layout_couples_removal_and_ignores_untouched_legacy_snapshots() {
        let mut edited = CacheSettings::default();
        edited.set_home_rows(Vec::new());
        let grouped = settings_fields(&edited)
            .into_iter()
            .find(|(key, _, _)| key == "home_layout")
            .map(|(key, value, _)| (key, value))
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&grouped.1).unwrap();
        assert_eq!(value["home_rows"], serde_json::json!([]));
        assert_eq!(value["home_recommendations_migrated"], true);
        assert!(!setting_can_override("home_recommendations_migrated"));

        let untouched = CacheSettings {
            home_rows: Some(vec![nova_config::HomeRow {
                source: HomeRowSource::Upcoming,
                enabled: true,
            }]),
            ..Default::default()
        };
        let old_records: HashMap<_, _> = settings_fields(&untouched)
            .into_iter()
            .map(|(key, value, _)| (key, value))
            .collect();
        assert!(old_records.contains_key("home_rows"));
        assert!(!old_records.contains_key("home_layout"));
        assert!(!old_records.contains_key("home_recommendations_migrated"));
        let legacy = vec![
            ("home_rows".into(), old_records["home_rows"].clone()),
            ("home_recommendations_migrated".into(), "false".into()),
        ];
        assert_eq!(
            shared_setting_value(&legacy, "home_rows"),
            Some(serde_json::to_value(&untouched.home_rows).unwrap())
        );
        for records in [
            vec![grouped.clone(), legacy[0].clone(), legacy[1].clone()],
            vec![legacy[1].clone(), legacy[0].clone(), grouped.clone()],
        ] {
            let merged = merge_settings_fields(&untouched, &records);
            assert!(merged.home_recommendations_migrated);
            assert!(merged.effective_home_rows().is_empty());
            assert_eq!(
                shared_setting_value(&records, "home_rows"),
                Some(serde_json::json!([]))
            );
            // A full settings publication from the second device may run
            // before its UI projection. It must retain the received group,
            // rather than tombstoning it or publishing the stale false bit.
            let republished: HashMap<_, _> = settings_fields_for_records(&untouched, &records)
                .into_iter()
                .map(|(key, value, _)| (key, value))
                .collect();
            assert_eq!(republished["home_layout"], grouped.1);
            assert!(!republished.contains_key("home_rows"));
            assert!(!republished.contains_key("home_recommendations_migrated"));
        }
    }

    #[test]
    fn home_layout_override_keeps_local_migration_and_publishes_account_pair() {
        let mut account = CacheSettings::default();
        account.set_home_rows(Vec::new());
        let records = settings_fields(&account)
            .into_iter()
            .filter(|(key, _, _)| key == "home_layout")
            .map(|(key, value, _)| (key, value))
            .collect::<Vec<_>>();
        let mut local = CacheSettings::default();
        local
            .sync_overrides
            .insert("home_rows".into(), serde_json::Value::Null);
        let merged = merge_settings_fields(&local, &records);
        assert!(!merged.home_recommendations_migrated);
        assert!(merged.home_row_enabled(&HomeRowSource::BecauseYouWatched));
        assert_eq!(merged.sync_overrides["home_rows"], serde_json::json!([]));
        let published: HashMap<_, _> = settings_fields_for_records(&merged, &records)
            .into_iter()
            .map(|(key, value, _)| (key, value))
            .collect();
        assert_eq!(published["home_layout"], records[0].1);
    }

    #[test]
    fn recommendation_dismissals_union_independent_devices_and_stale_publications() {
        let first = watch_history::title_key("movie", "tt123");
        let second = watch_history::title_key("series", "tmdb:456");
        let mut device_a = CacheSettings::default();
        device_a.home_recommendations_hidden.insert(first.clone());
        let mut device_b = CacheSettings::default();
        device_b.home_recommendations_hidden.insert(second.clone());
        let dismissal_records = |settings: &CacheSettings| {
            settings_fields(settings)
                .into_iter()
                .filter(|(key, _, _)| key.starts_with(RECOMMENDATION_HIDDEN_PREFIX))
                .map(|(key, value, _)| (key, value))
                .collect::<Vec<_>>()
        };
        let mut records = dismissal_records(&device_a);
        records.extend(dismissal_records(&device_b));
        assert_eq!(records.len(), 2);
        assert!(!setting_can_override("home_recommendations_hidden"));
        let expected: BTreeSet<_> = [first.clone(), second.clone()].into_iter().collect();
        for local in [&device_a, &device_b, &CacheSettings::default()] {
            let merged = merge_settings_fields(local, &records);
            assert_eq!(merged.home_recommendations_hidden, expected);
            let restored: CacheSettings =
                serde_json::from_str(&serde_json::to_string(&merged).unwrap()).unwrap();
            assert_eq!(restored.home_recommendations_hidden, expected);
            // A fresh or stale settings snapshot cannot tombstone a received
            // dismissal that its UI has not projected yet.
            let published: HashMap<_, _> = settings_fields_for_records(local, &records)
                .into_iter()
                .map(|(key, value, _)| (key, value))
                .collect();
            assert!(!published.contains_key("home_recommendations_hidden"));
            for identity in &expected {
                assert_eq!(
                    published[&format!("{RECOMMENDATION_HIDDEN_PREFIX}{identity}")],
                    "true"
                );
            }
        }

        // Older array snapshots migrate into per-identity records and cannot
        // clear identities even when their latest whole-array value is empty.
        let legacy = vec![(
            "home_recommendations_hidden".into(),
            serde_json::to_string(&vec![second.clone()]).unwrap(),
        )];
        let migrated = merge_settings_fields(&device_a, &legacy);
        assert_eq!(migrated.home_recommendations_hidden, expected);
        assert_eq!(dismissal_records(&migrated).len(), 2);
        let emptied = merge_settings_fields(
            &migrated,
            &[("home_recommendations_hidden".into(), "[]".into())],
        );
        assert_eq!(emptied.home_recommendations_hidden, expected);
        let malformed = vec![
            (
                format!("{RECOMMENDATION_HIDDEN_PREFIX}not-json"),
                "true".into(),
            ),
            (
                format!("{RECOMMENDATION_HIDDEN_PREFIX}{first}"),
                "false".into(),
            ),
            (
                format!(
                    "{RECOMMENDATION_HIDDEN_PREFIX}{}",
                    watch_history::title_key("movie", "")
                ),
                "true".into(),
            ),
        ];
        assert!(
            merge_settings_fields(&CacheSettings::default(), &malformed)
                .home_recommendations_hidden
                .is_empty()
        );
    }

    #[test]
    fn accent_color_syncs_and_respects_device_override() {
        use nova_config::AccentColor;
        let mut local = CacheSettings {
            accent_color: AccentColor::Orange,
            ..Default::default()
        };
        assert!(setting_can_override("accent_color"));
        let remote = [("accent_color".into(), "\"teal\"".into())];
        assert_eq!(
            merge_settings_fields(&local, &remote).accent_color,
            AccentColor::Teal
        );
        local
            .sync_overrides
            .insert("accent_color".into(), serde_json::json!("purple"));
        let merged = merge_settings_fields(&local, &remote);
        assert_eq!(merged.accent_color, AccentColor::Orange);
        assert_eq!(
            merged.sync_overrides["accent_color"],
            serde_json::json!("teal")
        );
        let published: HashMap<_, _> = settings_fields(&merged)
            .into_iter()
            .map(|(key, value, _)| (key, value))
            .collect();
        assert_eq!(published["accent_color"], "\"teal\"");
    }

    #[test]
    fn overrides_mask_publication_and_follow_shared_values_without_changing_local() {
        let mut local = CacheSettings {
            quality: 95,
            animations: false,
            ..Default::default()
        };
        local
            .sync_overrides
            .insert("quality".into(), serde_json::json!(80));
        local
            .sync_overrides
            .insert("animations".into(), serde_json::json!(true));
        let published: HashMap<_, _> = settings_fields(&local)
            .into_iter()
            .map(|(k, v, _)| (k, v))
            .collect();
        let cache: serde_json::Value = serde_json::from_str(&published["cache"]).unwrap();
        assert_eq!(cache["quality"], 80);
        assert_eq!(published["animations"], "true");
        assert!(!published.contains_key("sync_overrides"));
        let records = vec![
            ("cache".into(), r#"{"quality":60,"format":"jpeg"}"#.into()),
            ("animations".into(), "false".into()),
        ];
        let merged = merge_settings_fields(&local, &records);
        assert_eq!(merged.quality, 95);
        assert_eq!(merged.format, CacheImageFormat::Jpeg);
        assert!(!merged.animations);
        assert_eq!(merged.sync_overrides["quality"], 60);
        assert_eq!(merged.sync_overrides["animations"], false);
        let published: HashMap<_, _> = settings_fields(&merged)
            .into_iter()
            .map(|(k, v, _)| (k, v))
            .collect();
        let cache: serde_json::Value = serde_json::from_str(&published["cache"]).unwrap();
        assert_eq!(cache["quality"], 60);
        assert_eq!(cache["format"], "jpeg");
        let absent = merge_settings_fields(&local, &[]);
        assert_eq!(
            absent.sync_overrides["quality"], 80,
            "an absent shared field keeps its captured baseline"
        );
    }

    #[test]
    fn language_syncs_unless_this_device_has_an_override() {
        let mut local = CacheSettings {
            language: Language::Croatian,
            ..Default::default()
        };
        let published: HashMap<_, _> = settings_fields(&local)
            .into_iter()
            .map(|(k, v, _)| (k, v))
            .collect();
        assert_eq!(published["language"], "\"Croatian\"");
        let incoming = vec![("language".into(), "\"English\"".into())];
        assert_eq!(
            merge_settings_fields(&local, &incoming).language,
            Language::English
        );
        local
            .sync_overrides
            .insert("language".into(), serde_json::json!("English"));
        let merged = merge_settings_fields(&local, &incoming);
        assert_eq!(merged.language, Language::Croatian);
        assert_eq!(merged.sync_overrides["language"], "English");
        let published: HashMap<_, _> = settings_fields(&merged)
            .into_iter()
            .map(|(k, v, _)| (k, v))
            .collect();
        assert_eq!(published["language"], "\"English\"");
    }

    #[test]
    fn removing_override_membership_survives_forward_compatible_snapshot_merge() {
        let old = r#"{"quality":95,"sync_overrides":{"quality":80},"future":true}"#;
        let new = r#"{"quality":60,"sync_overrides":{}}"#;
        let merged: serde_json::Value =
            serde_json::from_str(&preserve_settings_snapshot(Some(old), new)).unwrap();
        assert_eq!(merged["sync_overrides"], serde_json::json!({}));
        assert_eq!(merged["quality"], 60);
        assert_eq!(merged["future"], true);
        assert!(setting_can_override("language"));
        assert!(setting_can_override("background_style"));
        assert!(!setting_can_override("not_a_setting"));
    }

    #[test]
    fn merge_settings_fields_unions_independent_and_group_records() {
        // Device A changed the cache group (whole-record), device B changed an
        // independent display setting; both survive because they are different
        // records.
        let base = CacheSettings::default();
        let records = vec![
            ("cache".to_string(), "{\"quality\":95}".to_string()),
            ("animations".to_string(), "false".to_string()),
        ];
        let merged = merge_settings_fields(&base, &records);
        assert_eq!(merged.quality, 95);
        assert!(!merged.animations);
        // Cache fields the record omitted fall back to the base.
        assert!(merged.cache_images);
    }

    #[test]
    fn merge_settings_fields_cache_group_is_lww_as_a_whole() {
        // A later cache record replaces the whole group, not individual fields.
        let base = CacheSettings::default();
        let records = vec![
            ("cache".to_string(), "{\"quality\":70}".to_string()),
            (
                "cache".to_string(),
                "{\"quality\":30,\"format\":\"jpeg\"}".to_string(),
            ),
        ];
        let merged = merge_settings_fields(&base, &records);
        assert_eq!(merged.quality, 30);
        assert_eq!(merged.format, CacheImageFormat::Jpeg);
    }

    #[test]
    fn legacy_cache_group_migrates_player_resolution_but_explicit_value_wins() {
        let base = CacheSettings::default();
        let migrated =
            merge_settings_fields(&base, &[("cache".into(), r#"{"downscale":true}"#.into())]);
        assert_eq!(migrated.player_episode_backdrop_max_side, Some(1024));
        let explicit_original = merge_settings_fields(
            &base,
            &[(
                "cache".into(),
                r#"{"downscale":true,"player_episode_backdrop_max_side":null}"#.into(),
            )],
        );
        assert_eq!(explicit_original.player_episode_backdrop_max_side, None);
        let explicit_value = merge_settings_fields(
            &base,
            &[(
                "cache".into(),
                r#"{"downscale":true,"player_episode_backdrop_max_side":720}"#.into(),
            )],
        );
        assert_eq!(explicit_value.player_episode_backdrop_max_side, Some(720));
    }

    #[test]
    fn merge_settings_fields_prefers_remote_and_keeps_local_device_fields() {
        let base = CacheSettings {
            android_hwdec: AndroidHwdec::Sw,
            desktop_hwdec: nova_config::DesktopHwdec::Sw,
            player_external: true,
            remote_control_enabled: false,
            desktop_external_app: DesktopExternalApp::Mpv,
            home_catalog_sources: vec![HomeCatalogSource {
                addon_url: "https://local.test/manifest.json".into(),
                type_: "series".into(),
                catalog_id: "local".into(),
                genre: String::new(),
            }],
            ..Default::default()
        };
        let remote_home_sources = vec![HomeCatalogSource {
            addon_url: "https://remote.test/manifest.json".into(),
            type_: "series".into(),
            catalog_id: "trending".into(),
            genre: "Action".into(),
        }];
        let records = vec![
            ("cache".to_string(), "{\"quality\":77}".to_string()),
            ("android_hwdec".to_string(), "\"hw+\"".to_string()),
            ("desktop_hwdec".to_string(), "\"hw+\"".to_string()),
            ("player_external".to_string(), "false".to_string()),
            ("remote_control_enabled".to_string(), "true".to_string()),
            ("desktop_external_app".to_string(), "\"system\"".to_string()),
            (
                "home_catalog_sources".to_string(),
                serde_json::to_string(&remote_home_sources).unwrap(),
            ),
        ];
        let merged = merge_settings_fields(&base, &records);
        assert_eq!(merged.quality, 77);
        // Device-specific fields are never taken from the remote record.
        assert_eq!(merged.android_hwdec, AndroidHwdec::Sw);
        assert_eq!(merged.desktop_hwdec, nova_config::DesktopHwdec::Sw);
        assert!(merged.player_external);
        assert!(!merged.remote_control_enabled);
        assert_eq!(merged.desktop_external_app, DesktopExternalApp::Mpv);
        assert_eq!(merged.home_catalog_sources, remote_home_sources);
    }

    #[test]
    fn merge_settings_fields_group_cannot_smuggle_other_fields() {
        // A cache record may only carry the cache group's own fields.
        let base = CacheSettings::default();
        let records = vec![(
            "cache".to_string(),
            "{\"quality\":55,\"animations\":false}".to_string(),
        )];
        let merged = merge_settings_fields(&base, &records);
        assert_eq!(merged.quality, 55);
        assert!(merged.animations);
    }

    #[test]
    fn merge_settings_fields_ignores_unknown_keys() {
        let base = CacheSettings {
            quality: 42,
            ..Default::default()
        };
        let records = vec![("not_a_field".to_string(), "true".to_string())];
        let merged = merge_settings_fields(&base, &records);
        assert_eq!(merged.quality, 42);
        assert!(merged.animations);
    }
}

#[cfg(test)]
mod addon_capability_sync_tests {
    use super::{SyncedAddon, merged_addon_capabilities};
    use nova_providers::ProviderCapabilities;
    #[test]
    fn older_peer_omission_preserves_local_choices_but_explicit_defaults_reset() {
        let old: SyncedAddon =
            serde_json::from_str(r#"{"enabled":true,"label":"AniKoto"}"#).unwrap();
        let streams_only = ProviderCapabilities {
            streams: true,
            metadata: false,
        };
        assert_eq!(
            merged_addon_capabilities(old.capabilities, Some(streams_only)),
            streams_only
        );
        assert_eq!(
            merged_addon_capabilities(old.capabilities, None),
            ProviderCapabilities::default()
        );
        let explicit: SyncedAddon = serde_json::from_str(
            r#"{"enabled":true,"capabilities":{"streams":true,"metadata":true}}"#,
        )
        .unwrap();
        assert_eq!(
            merged_addon_capabilities(explicit.capabilities, Some(streams_only)),
            ProviderCapabilities::default()
        );
    }
}

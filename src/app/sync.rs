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
pub(crate) const DOMAIN_ADDONS: &str = "addons";
pub(crate) const DOMAIN_SETTINGS: &str = "settings";
pub(crate) const DOMAIN_CATEGORY: &str = "category";
/// Continue Watching removals, one record per item id (`hidden_at` unix secs).
/// App-side only: a new domain name travels as a string, so peers that predate
/// it simply ignore it.
pub(crate) const DOMAIN_CONTINUE_HIDDEN: &str = "continue_hidden";

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
        self.0.lock().unwrap().records(domain)
    }
}
fn projection_store() -> Option<ProjectionStore> {
    nova_sync::local_store().ok().map(ProjectionStore)
}

/// Wire format for one synced addon entry. `label` travels so every device
/// shows the same name (empty from older peers, then derived locally).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SyncedAddon {
    #[serde(default)]
    label: String,
    enabled: bool,
    #[serde(default)]
    configure_ok: Option<bool>,
}

// ---------------------------------------------------------------------------
// Decompose: push local writes into the sync store
// ---------------------------------------------------------------------------

/// Insert/update `records` and tombstone any live key of `domain` not
/// present in the list, as one batched store write (a seed of a large library
/// would otherwise save once per record).
fn sync_records(domain: &str, records: Vec<(String, String, u64)>) {
    let key = match domain {
        DOMAIN_LIBRARY => "library",
        DOMAIN_PROGRESS => "episode_progress",
        DOMAIN_ADDONS => "addons",
        DOMAIN_SETTINGS | DOMAIN_CATEGORY => "settings",
        DOMAIN_CONTINUE_HIDDEN => "continue_hidden",
        _ => "",
    };
    if !writable_key(key) {
        return;
    }
    if let Err(e) = nova_sync::commit_snapshot(domain, &records, None, applying(), false) {
        storage::report(storage::Error::new(storage::ErrorKind::Transaction, e));
    }
}

/// App snapshots and record mutations share one transaction, even with sync
/// disabled. A persisted baseline is the only deletion authority.
pub(crate) fn persist_sync_snapshot(key: &str, raw: &str, seed: bool) -> nova_sync::Result<()> {
    if !writable_key(key) {
        return Err(storage::Error::new(
            storage::ErrorKind::Schema,
            "unreadable snapshot retained",
        )
        .into());
    }
    let domains: Vec<(&str, Vec<(String, String, u64)>)> = match key {
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
        "episode_progress" => {
            let map: HashMap<String, EpisodeProgress> = serde_json::from_str(raw)?;
            vec![(
                DOMAIN_PROGRESS,
                map.iter()
                    .map(|(k, v)| Ok((k.clone(), serde_json::to_string(v)?, v.updated_at_secs)))
                    .collect::<Result<_, serde_json::Error>>()?,
            )]
        }
        "continue_hidden" => {
            let map: HashMap<String, u64> = serde_json::from_str(raw)?;
            vec![(
                DOMAIN_CONTINUE_HIDDEN,
                map.into_iter()
                    .map(|(k, v)| (k, v.to_string(), v))
                    .collect(),
            )]
        }
        "addons" => {
            let addons: Vec<AddonStore> = serde_json::from_str(raw)?;
            let mut records = addons
                .iter()
                .map(|a| {
                    Ok((
                        a.url.clone(),
                        serde_json::to_string(&SyncedAddon {
                            label: a.label.clone(),
                            enabled: a.enabled,
                            configure_ok: None,
                        })?,
                        0,
                    ))
                })
                .collect::<Result<Vec<_>, serde_json::Error>>()?;
            records.push(addon_order_record(&addons));
            vec![(DOMAIN_ADDONS, records)]
        }
        "settings" => {
            let settings: CacheSettings = serde_json::from_str(raw)?;
            vec![
                (DOMAIN_SETTINGS, settings_fields(&settings)),
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
            storage::try_set_str(key, raw)?;
            return Ok(());
        }
    };
    let owner = nova_sync::local_store()?;
    let dev = nova_sync::local_device()?;
    let mut store = owner.lock().unwrap();
    for (domain, records) in domains {
        // Untouched installation defaults are projection-only. Legacy installs
        // with an existing settings object conservatively retain all choices.
        let untouched =
            seed && domain == DOMAIN_SETTINGS && storage::try_get_str("settings")?.is_none();
        nova_sync::prepare_snapshot(
            &mut store,
            dev,
            domain,
            &records,
            None,
            applying() || untouched,
            seed,
        )?;
    }
    let old = store.extra_value(key)?;
    let raw = if key == "settings" {
        preserve_unsupported_settings(old.as_deref(), raw)
    } else {
        raw.to_string()
    };
    // Map entries are user data, not unknown schema fields: retaining absent
    // keys here would resurrect projected progress/hide deletions on restart.
    let raw = if key == "settings" {
        nova_sync::preserve_unknown(old.as_deref(), &raw)
    } else {
        raw
    };
    store.queue_extra(key, Some(raw));
    store.save()
}

pub(crate) fn notify_library(entries: &[LibraryEntry]) {
    let records = entries
        .iter()
        .filter_map(|e| {
            serde_json::to_string(e)
                .ok()
                .map(|json| (e.id.clone(), json, 0))
        })
        .collect();
    sync_records(DOMAIN_LIBRARY, records);
}

pub(crate) fn notify_progress(map: &HashMap<String, EpisodeProgress>) {
    let records = map
        .iter()
        .filter_map(|(key, progress)| {
            serde_json::to_string(progress)
                .ok()
                .map(|json| (key.clone(), json, progress.updated_at_secs))
        })
        .collect();
    sync_records(DOMAIN_PROGRESS, records);
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
/// (`android_hwdec`, `player_external`, `desktop_external_app`,
/// `playback_speed`, `language`) or local-only (`rewrite_existing`);
/// `categories` sync as their own domain so concurrent additions union.
const UNSYNCED_SETTINGS_FIELDS: &[&str] = &[
    "android_hwdec",
    "player_external",
    "desktop_external_app",
    "playback_speed",
    "episode_start_behavior",
    "language",
    "rewrite_existing",
    "categories",
];

/// Image-cache controls are interdependent (re-encoding only applies to the
/// chosen format/quality/downscale, and "re-encode on load" rewrites entries
/// under that config), so they sync as one whole record under `settings/cache`
/// with whole-value LWW instead of field-by-field.
const SETTINGS_GROUP_CACHE: &[&str] = &[
    "cache_images",
    "enabled",
    "format",
    "quality",
    "downscale",
    "lazy_reencode",
    "lru_cache_mb",
];

/// Settings fields synced as one whole-record blob: `(record key, field names)`.
const SYNCED_SETTINGS_GROUPS: &[(&str, &[&str])] = &[("cache", SETTINGS_GROUP_CACHE)];

/// The group record that carries `field`, if any.
fn settings_group_of(field: &str) -> Option<&'static str> {
    SYNCED_SETTINGS_GROUPS
        .iter()
        .find(|(_, fields)| fields.contains(&field))
        .map(|(key, _)| *key)
}

/// The field names carried by the group record key `record`, if it is one.
fn settings_group_fields(record: &str) -> Option<&'static [&'static str]> {
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
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut groups: HashMap<&str, serde_json::Map<String, serde_json::Value>> = HashMap::new();
    for (name, value) in map {
        if UNSYNCED_SETTINGS_FIELDS.contains(&name.as_str()) {
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

/// Rebuild settings by overlaying records onto `base` (the local settings).
/// A group record replaces every field it carries, so the group moves as one
/// LWW value. Device-specific fields always keep the local value; unknown
/// fields are ignored by serde, so a newer peer can add settings safely.
fn merge_settings_fields(base: &CacheSettings, fields: &[(String, String)]) -> CacheSettings {
    let Ok(serde_json::Value::Object(mut map)) = serde_json::to_value(base) else {
        return base.clone();
    };
    for (name, json) in fields {
        if UNSYNCED_SETTINGS_FIELDS.contains(&name.as_str()) {
            continue;
        }
        if let Some(allowed) = settings_group_fields(name) {
            if let Ok(serde_json::Value::Object(group)) =
                serde_json::from_str::<serde_json::Value>(json)
            {
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
    let mut merged: CacheSettings =
        serde_json::from_value(serde_json::Value::Object(map)).unwrap_or_else(|_| base.clone());
    // Device-specific fields are never taken from the remote record.
    merged.android_hwdec = base.android_hwdec;
    merged.player_external = base.player_external;
    merged.desktop_external_app = base.desktop_external_app;
    merged.rewrite_existing = false;
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
    sync_records(DOMAIN_SETTINGS, settings_fields(settings));
    let categories = settings
        .categories
        .iter()
        .map(|name| (name.clone(), "1".to_string(), 0))
        .collect();
    sync_records(DOMAIN_CATEGORY, categories);
}

pub(crate) fn notify_setting_choice(field: &str, settings: &CacheSettings) {
    let key = settings_group_of(field).unwrap_or(field);
    if let Some((key, value, _)) = settings_fields(settings)
        .into_iter()
        .find(|(k, _, _)| k == key)
    {
        let result = (|| -> nova_sync::Result<()> {
            let dev = nova_sync::local_device()?;
            let owner = nova_sync::local_store()?;
            let mut store = owner.lock().unwrap();
            let value = nova_sync::preserve_unknown(
                store
                    .record(DOMAIN_SETTINGS, &key)
                    .and_then(|r| r.value.as_deref()),
                &value,
            );
            store.set(DOMAIN_SETTINGS, &key, Some(value), 0, dev);
            store.save()
        })();
        if let Err(e) = result {
            storage::report(storage::Error::new(storage::ErrorKind::Transaction, e));
        }
    }
}

// ---------------------------------------------------------------------------
// Materialize: rebuild app state from remote records
// ---------------------------------------------------------------------------

impl Bridge {
    /// Push all current local state into the sync store. Called once at
    /// startup/after enabling; a no-op for records that already match.
    pub(super) fn sync_seed(&self) {
        let (entries, progress, hidden, addons, settings) = {
            let state = self.shared.lock().unwrap();
            (
                state.entries.clone(),
                state.progress.clone(),
                state.continue_hidden.clone(),
                state
                    .installed
                    .iter()
                    .map(|a| AddonStore {
                        url: a.url.clone(),
                        enabled: a.enabled,
                        configure_ok: a.configure_ok,
                        label: a.label.clone(),
                    })
                    .collect::<Vec<_>>(),
                state.cache_settings.clone(),
            )
        };
        for (key, domain, raw) in [
            ("library", DOMAIN_LIBRARY, serde_json::to_string(&entries)),
            (
                "episode_progress",
                DOMAIN_PROGRESS,
                serde_json::to_string(&progress),
            ),
            (
                "continue_hidden",
                DOMAIN_CONTINUE_HIDDEN,
                serde_json::to_string(&hidden),
            ),
            ("addons", DOMAIN_ADDONS, serde_json::to_string(&addons)),
            (
                "settings",
                DOMAIN_SETTINGS,
                serde_json::to_string(&settings),
            ),
        ] {
            let result = (|| -> nova_sync::Result<()> {
                if storage::try_get_str(&format!("sync:baseline:{domain}"))?.is_none() {
                    persist_sync_snapshot(key, &raw?, true)?;
                }
                Ok(())
            })();
            if let Err(e) = result {
                storage::report(storage::Error::new(storage::ErrorKind::Transaction, e));
            }
        }
    }

    pub(super) fn replay_sync_projection(&self) {
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
                self.sync_apply(pending);
            }
        }
    }

    /// Apply remote records for the given domains. Runs on the UI thread.
    pub(super) fn sync_apply(&self, domains: Vec<String>) {
        let _guard = ApplyingGuard::new();
        let peers_changed = domains.iter().any(|d| d == nova_sync::DOMAIN_PEERS);
        for domain in domains {
            let failures = write_failures();
            let basis = nova_sync::local_store()
                .ok()
                .and_then(|s| s.lock().unwrap().digest().get(&domain).cloned());
            match domain.as_str() {
                DOMAIN_LIBRARY => self.sync_apply_library(),
                DOMAIN_PROGRESS => self.sync_apply_progress(),
                DOMAIN_ADDONS => self.sync_apply_addons(),
                DOMAIN_SETTINGS | DOMAIN_CATEGORY => self.sync_apply_settings(),
                DOMAIN_CONTINUE_HIDDEN => self.sync_apply_continue_hidden(),
                _ => {}
            }
            if let (Ok(owner), Some(basis)) = (
                nova_sync::local_store(),
                basis.filter(|_| write_failures() == failures),
            ) {
                if let Err(e) = owner.lock().unwrap().mark_projected(&domain, &basis) {
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
        if peers_changed {
            // The mesh grew or shrank: refresh the list and immediately run a
            // pass so newly learned devices are dialed now. `Notify` coalesces
            // these, so calling it directly cannot storm.
            self.sync_status_to_ui();
            if let Some(engine) = nova_sync::engine() {
                // A peer removed us from its sync: say so instead of silently
                // dropping it from the list.
                if let Some(name) = engine.take_removed_notice()
                    && let Some(app) = self.app()
                {
                    let name = if name.trim().is_empty() {
                        text::tr("another device").to_string()
                    } else {
                        name
                    };
                    app.set_sync_link_notice(SharedString::from(text::removed_from_sync(&name)));
                }
                engine.sync_now();
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
        self.apply_library_to_ui();
        self.rebuild_continue_list();
        self.rebuild_upcoming_list();
        self.apply_home_to_ui();
        self.dispatch_continue_posters();
        self.dispatch_upcoming_posters();
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
        self.rebuild_continue_list();
        self.rebuild_upcoming_list();
        self.update_library_badges();
        // An open detail modal shows episode rows too: refresh them on the
        // light path (no thumbnail re-queue) so remote progress lands there.
        self.apply_episode_rows();
        self.apply_home_to_ui();
        self.dispatch_continue_posters();
        self.dispatch_upcoming_posters();
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
        self.rebuild_continue_list();
        self.apply_home_to_ui();
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
                    .position(|a| !desired.iter().any(|d| &d.url == &a.url))
            };
            match stale {
                Some(idx) => self.remove_addon_at(idx),
                None => break,
            }
        }
        // Install missing ones (uses the cached manifest, else fetches) with
        // the synced label.
        for addon in &desired {
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

    fn sync_apply_settings(&self) {
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
        write_settings(&settings);
        self.settings_to_ui();
    }
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
        match nova_sync::foreground_engine() {
            Ok(engine) => {
                // Remote changes arrive on the tokio runtime thread; marshal
                // them onto the UI thread before touching app state.
                let b = self.clone();
                engine.set_on_remote(std::sync::Arc::new(move |domains| {
                    let b = b.clone();
                    let _ = slint::invoke_from_event_loop(move || b.sync_apply(domains));
                }));
                // Pairing events (requests + completions) likewise marshal.
                let b = self.clone();
                engine.set_pair_callback(std::sync::Arc::new(move |event| {
                    let b = b.clone();
                    let _ = slint::invoke_from_event_loop(move || b.sync_pair_event(event));
                }));
                // Keep the Android periodic job in step with the enable +
                // background flags: it runs a bounded pass with no Activity.
                #[cfg(target_os = "android")]
                crate::app::android_bg::set_periodic_sync(
                    nova_sync::read_settings().background_enabled,
                );
                // A device that has synced before gets the merged state from
                // the store first; seeding then captures local-only records.
                let domains = nova_sync::engine().map(|e| e.domains()).unwrap_or_default();
                if !domains.is_empty() {
                    self.sync_apply(domains);
                }
                self.sync_status_to_ui();
                if let Some(engine) = nova_sync::engine() {
                    engine.sync_now();
                }
            }
            Err(e) => {
                tracing::error!(target: "nova_sync::app", error = %format_args!("{e:#}"), "could not start sync");
                self.sync_status_to_ui();
            }
        }
    }

    /// Stop the sync engine and drop its endpoint.
    pub(super) fn stop_sync(&self) {
        #[cfg(target_os = "android")]
        crate::app::android_bg::set_periodic_sync(false);
        nova_sync::uninstall();
        self.sync_status_to_ui();
    }
}

impl Bridge {
    /// Refresh the Settings → Sync readout from the engine (enable flag,
    /// identity, peers, status line).
    pub(super) fn sync_status_to_ui(&self) {
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
                app.set_sync_status(SharedString::from(sync_status_text(&engine.status())));
            }
            None => {
                app.set_sync_identity(SharedString::default());
                app.set_sync_peers(Rc::new(VecModel::from(Vec::<SyncPeer>::new())).into());
                app.set_sync_invites(Rc::new(VecModel::from(Vec::<SyncInvite>::new())).into());
                app.set_sync_status(SharedString::default());
            }
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
                    app.set_sync_link_notice(SharedString::from(text::paired_with(&name)));
                }
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
        if nova_sync::is_running() {
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
                    app.set_sync_link_notice(SharedString::from(text::tr(
                        "Invite ready — share the code; it expires in 15 minutes.",
                    )));
                }
            }
            Err(e) => {
                if let Some(app) = self.app() {
                    app.set_sync_link_notice(SharedString::from(text::could_not_create_invite(
                        &e.to_string(),
                    )));
                }
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
                Ok(()) => app.set_sync_link_notice(SharedString::from(text::tr(
                    "Connecting to the other device…",
                ))),
                Err(e) => app
                    .set_sync_link_notice(SharedString::from(text::invalid_invite(&e.to_string()))),
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
            if let Some(app) = self.app() {
                app.set_sync_link_notice(SharedString::from(text::tr(
                    "Camera scanning is available on Android only.",
                )));
            }
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

/// Relative age ("5s ago", "3m ago", "2h ago") of a past unix-secs
/// timestamp. Shared by the status line and the per-peer last-seen labels.
pub(crate) fn format_ago(secs: u64) -> String {
    text::ago(now_secs().saturating_sub(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            sort_by_order(current.clone(), &vec!["c".to_string(), "a".to_string()]),
            vec!["c".to_string(), "a".to_string(), "b".to_string()]
        );
        // Unknown urls keep trailing relative order; stale entries drop out.
        assert_eq!(
            sort_by_order(current.clone(), &vec!["c".to_string(), "z".to_string()]),
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
    fn settings_fields_split_per_field_and_group_cache() {
        let settings = CacheSettings {
            quality: 91,
            animations: false,
            categories: vec!["Anime".into()],
            android_hwdec: AndroidHwdec::Sw,
            player_external: true,
            desktop_external_app: DesktopExternalApp::Vlc,
            rewrite_existing: true,
            playback_speed: 1.5,
            ..Default::default()
        };

        let fields: HashMap<String, String> = settings_fields(&settings)
            .into_iter()
            .map(|(name, json, _)| (name, json))
            .collect();
        // Independent settings sync as their own records.
        assert_eq!(fields.get("animations").map(String::as_str), Some("false"));
        // Cache settings are coupled into one whole-record blob.
        let cache: serde_json::Value =
            serde_json::from_str(fields.get("cache").expect("cache group")).unwrap();
        assert_eq!(cache["quality"], 91);
        assert_eq!(cache["cache_images"], true);
        assert!(!fields.contains_key("quality"));
        // Local-only fields never sync here: the playback rate belongs to this
        // device (its speakers / headphones), not to the account.
        assert!(!fields.contains_key("categories"));
        assert!(!fields.contains_key("android_hwdec"));
        assert!(!fields.contains_key("player_external"));
        assert!(!fields.contains_key("desktop_external_app"));
        assert!(!fields.contains_key("playback_speed"));
        assert!(!fields.contains_key("rewrite_existing"));
    }

    #[test]
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
    fn merge_settings_fields_prefers_remote_and_keeps_local_device_fields() {
        let base = CacheSettings {
            android_hwdec: AndroidHwdec::Sw,
            player_external: true,
            desktop_external_app: DesktopExternalApp::Mpv,
            ..Default::default()
        };
        let records = vec![
            ("cache".to_string(), "{\"quality\":77}".to_string()),
            ("android_hwdec".to_string(), "\"hw+\"".to_string()),
            ("player_external".to_string(), "false".to_string()),
            ("desktop_external_app".to_string(), "\"system\"".to_string()),
            ("rewrite_existing".to_string(), "true".to_string()),
        ];
        let merged = merge_settings_fields(&base, &records);
        assert_eq!(merged.quality, 77);
        // Device-specific fields are never taken from the remote record.
        assert_eq!(merged.android_hwdec, AndroidHwdec::Sw);
        assert!(merged.player_external);
        assert_eq!(merged.desktop_external_app, DesktopExternalApp::Mpv);
        assert!(!merged.rewrite_existing);
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

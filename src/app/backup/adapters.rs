//! Each selector owns disjoint semantic records, including settings subfields.
use super::archive::Payload;
use crate::app::{self, profiles::ProfileStore};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const COMPONENTS: &[(&str, &str)] = &[
    ("library", "Library"),
    ("categories", "Categories"),
    ("progress", "Playback progress"),
    ("journal", "Viewing journal"),
    ("activity", "Viewing activity"),
    ("continue_hidden", "Hidden Continue rows"),
    ("history_hidden", "Hidden history rows"),
    ("episodes_hidden", "Hidden new episodes"),
    ("settings", "Personal settings and Home"),
    ("collections", "Smart collections"),
    ("bookmarks", "Scene bookmarks"),
    ("preferences", "Title playback preferences"),
    ("queue", "Watch queue"),
    ("addons", "Addons and configuration"),
    ("packages", "Optional addon package files"),
    ("integrations", "Integration accounts and mappings"),
    ("reminders", "Release reminders"),
    ("offline", "Smart offline policies"),
    ("policy", "Profile PIN and restrictions"),
    ("search", "Search history"),
    ("device", "This device's compatible preferences"),
];
pub(super) fn supported(name: &str) -> bool {
    COMPONENTS.iter().any(|(id, _)| *id == name)
}

// Paths/decoder choices and installation capabilities stay on their device.
const DEVICE_FIELDS: &[&str] = &[
    "android_hwdec",
    "desktop_hwdec",
    "player_external",
    "desktop_external_app",
    "android_auto_continue",
    "android_pip_enabled",
    "android_auto_rotate",
    "interface_mode",
    "min_grid_columns",
    "lru_cache_mb",
];
fn portable_setting(key: &str) -> bool {
    key != "categories"
        && key != "smart_collections"
        && key != "tmdb_api_key"
        && key != "sync_overrides"
        && !DEVICE_FIELDS.contains(&key)
        && !key.contains("path")
        && !key.contains("folder")
        && !key.starts_with("sync_")
        && key != "remote_control_enabled"
}

fn decode(raw: Option<&String>, fallback: Value) -> Result<Value, String> {
    raw.map(|raw| serde_json::from_str(raw).map_err(|e| e.to_string()))
        .unwrap_or(Ok(fallback))
}
fn split(key: &str) -> Result<(&str, &str), String> {
    key.split_once('\0')
        .ok_or_else(|| "Invalid component record identity".into())
}
fn insert_map(out: &mut Payload, key: &str, value: Value) -> Result<(), String> {
    let map = value.as_object().ok_or("Invalid component map")?;
    for (id, value) in map {
        out.records.insert(format!("{key}\0{id}"), value.clone());
    }
    Ok(())
}
fn insert_array(
    out: &mut Payload,
    key: &str,
    value: Value,
    identity: impl Fn(&Value) -> Option<String>,
) -> Result<(), String> {
    for value in value.as_array().ok_or("Invalid component list")? {
        let id = identity(value).ok_or("Missing component entity identity")?;
        if out
            .records
            .insert(format!("{key}\0{id}"), value.clone())
            .is_some()
        {
            return Err("Duplicate component entity identity".into());
        }
    }
    Ok(())
}
fn map_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "progress" => "episode_progress",
        "activity" => "viewing_activity:v1",
        "continue_hidden" => "continue_hidden",
        "history_hidden" => "watch_history_hidden:v1",
        "episodes_hidden" => "new_episodes_hidden:v1",
        "offline" => "offline_policies:v1",
        _ => return None,
    })
}
fn queue_payload(value: Value) -> Result<Payload, String> {
    app::watch_queue::records(&value.to_string()).map_err(|e| e.to_string())?;
    let mut out = Payload {
        version: 1,
        ..Payload::default()
    };
    let mut extras = value.as_object().cloned().ok_or("Invalid queue")?;
    for key in ["version", "entries", "order", "cursor"] {
        extras.remove(key);
    }
    out.records
        .insert("queue_settings\0value".into(), json!(extras));
    for key in ["order", "cursor"] {
        if let Some(value) = value.get(key) {
            out.records
                .insert(format!("queue_{key}\0value"), value.clone());
        }
    }
    for entry in value["entries"].as_array().ok_or("Invalid queue entries")? {
        let id = json!([
            entry["type_"].as_str().ok_or("Invalid queue type")?,
            entry["series_id"].as_str().ok_or("Invalid queue title")?,
            entry["episode_id"]
                .as_str()
                .ok_or("Invalid queue episode")?
        ])
        .to_string();
        out.records
            .insert(format!("queue_items\0{id}"), entry.clone());
    }
    Ok(out)
}
fn queue_snapshot(payload: &Payload) -> Value {
    let mut value = payload
        .records
        .get("queue_settings\0value")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    value.insert("version".into(), json!(2));
    value.insert(
        "entries".into(),
        json!(
            payload
                .records
                .iter()
                .filter(|(key, _)| key.starts_with("queue_items\0"))
                .map(|(_, value)| value.clone())
                .collect::<Vec<_>>()
        ),
    );
    value.insert(
        "order".into(),
        payload
            .records
            .get("queue_order\0value")
            .cloned()
            .unwrap_or(json!([])),
    );
    value.insert("cursor".into(),payload.records.get("queue_cursor\0value").cloned().unwrap_or(json!({"item_key":null,"last_completed_key":null,"phase":"ready","origin_device":0})));
    json!(value)
}
fn integration_record(domain: &str, key: &str) -> bool {
    match domain {
        "tracking" => {
            key.starts_with("link:")
                || key.starts_with("automatic:")
                || key.starts_with("credentials:")
        }
        "stremio" => {
            key == "connection"
                || key.starts_with("mapping:")
                || key.starts_with("mode:")
                || key.starts_with("automatic:")
                || key.starts_with("account:") && key.ends_with(":mode")
                || key.starts_with("session:") && key.ends_with(":automatic")
                || key.starts_with("selection:")
        }
        "list_services" => {
            key.starts_with("connection:")
                || key.starts_with("link:")
                || key.starts_with("mapping:")
                || key.starts_with("automatic:")
                || key.starts_with("mode:")
                || key.starts_with("account:")
                || key.starts_with("identity:")
                || key.starts_with("selected:")
        }
        _ => false,
    }
}

pub(super) fn capture(
    name: &str,
    owner: &ProfileStore,
    rows: &BTreeMap<String, String>,
    store: &nova_sync::Store,
) -> Result<Payload, String> {
    let mut out = Payload {
        version: 1,
        ..Payload::default()
    };
    if let Some(key) = map_key(name) {
        insert_map(&mut out, key, decode(rows.get(key), json!({}))?)?;
        return Ok(out);
    }
    let settings = || decode(rows.get("settings"), json!({}));
    match name {
        "settings" | "device" => {
            for (key, value) in settings()?.as_object().ok_or("Invalid settings")? {
                if (name == "settings" && portable_setting(key))
                    || (name == "device"
                        && DEVICE_FIELDS.contains(&key.as_str())
                        && (value.is_number() || value.is_boolean() || value.is_string()))
                {
                    out.records
                        .insert(format!("settings\0{key}"), value.clone());
                }
            }
            if name == "settings" {
                for key in ["home:showcase:v1", "home:catalog-rows:v1"] {
                    if let Some(raw) = rows.get(key) {
                        out.records.insert(
                            format!("{key}\0value"),
                            serde_json::from_str(raw).map_err(|e| e.to_string())?,
                        );
                    }
                }
            }
        }
        "preferences" => {
            insert_map(
                &mut out,
                "playback_preferences:v1",
                decode(rows.get("playback_preferences:v1"), json!({}))?,
            )?;
            let raw = rows
                .get(app::last_stream::FAMILY_KEY)
                .map(|raw| app::last_stream::portable_families(raw))
                .unwrap_or_else(|| app::last_stream::backup_families(owner))?;
            insert_map(
                &mut out,
                app::last_stream::FAMILY_KEY,
                serde_json::from_str(&raw).map_err(|e| e.to_string())?,
            )?;
        }
        "categories" => {
            let values = settings()?.get("categories").cloned().unwrap_or(json!([]));
            for name in values.as_array().ok_or("Invalid categories")? {
                let name = name.as_str().ok_or("Invalid category name")?;
                out.records
                    .insert(format!("categories\0{name}"), json!(name));
            }
        }
        "collections" => insert_array(
            &mut out,
            "smart_collections",
            settings()?
                .get("smart_collections")
                .cloned()
                .unwrap_or(json!([])),
            |v| v["id"].as_str().map(str::to_owned),
        )?,
        "library" => insert_array(
            &mut out,
            "library",
            decode(rows.get("library"), json!([]))?,
            |v| {
                Some(
                    json!([
                        v["type_"].as_str().or_else(|| v["type"].as_str())?,
                        v["id"].as_str()?
                    ])
                    .to_string(),
                )
            },
        )?,
        "bookmarks" => {
            let raw = rows
                .get("scene_bookmarks:v1")
                .map(String::as_str)
                .unwrap_or("{}");
            for (id, raw, _) in
                app::activity_sync::bookmark_records(raw).map_err(|e| e.to_string())?
            {
                out.records.insert(
                    format!("scene_bookmarks:v1\0{id}"),
                    serde_json::from_str(&raw).map_err(|e| e.to_string())?,
                );
            }
        }
        "journal" => {
            for (key, raw) in rows.range("viewing_journal:v1:".to_owned()..) {
                if !key.starts_with("viewing_journal:v1:") {
                    break;
                }
                let id =
                    app::viewing_journal::validate_session(key, raw).map_err(|e| e.to_string())?;
                out.records.insert(
                    format!("{key}\0{id}"),
                    serde_json::from_str(raw).map_err(|e| e.to_string())?,
                );
            }
        }
        "queue" => {
            if let Some(raw) = rows.get(app::watch_queue::KEY) {
                out = queue_payload(serde_json::from_str(raw).map_err(|e| e.to_string())?)?;
            }
        }
        "policy" => {
            if let Some(raw) = rows.get(app::profile_policy::KEY) {
                let policy: app::profile_policy::ProfilePolicy =
                    serde_json::from_str(raw).map_err(|e| e.to_string())?;
                policy.validate()?;
                out.records.insert(
                    "profile_policy:v1\0value".into(),
                    serde_json::from_str(raw).map_err(|e| e.to_string())?,
                );
            }
        }
        "addons" => {
            let value = decode(rows.get("addons"), json!([]))?;
            let order = value
                .as_array()
                .ok_or("Invalid addons")?
                .iter()
                .map(|value| value["url"].clone())
                .collect::<Vec<_>>();
            insert_array(&mut out, "addons", value, |v| {
                v["url"].as_str().map(str::to_owned)
            })?;
            out.records
                .insert("addon_order\0value".into(), json!(order));
            insert_map(
                &mut out,
                "plugin_settings",
                decode(rows.get("plugin_settings"), json!({}))?,
            )?;
            insert_map(
                &mut out,
                "addon_packages:v1",
                decode(rows.get("addon_packages:v1"), json!({}))?,
            )?;
        }
        "packages" => insert_map(
            &mut out,
            "package_files",
            decode(rows.get("addon_packages:v1"), json!({}))?,
        )?,
        "integrations" => {
            if let Some(value) = settings()?.get("tmdb_api_key") {
                out.records
                    .insert("settings\0tmdb_api_key".into(), value.clone());
            }
            for domain in ["tracking", "stremio", "list_services"] {
                for (key, raw) in store.records(&owner.domain(domain)) {
                    if integration_record(domain, &key) {
                        out.records.insert(
                            format!("@{domain}\0{key}"),
                            serde_json::from_str(&raw).map_err(|e| e.to_string())?,
                        );
                    }
                }
            }
        }
        "reminders" => {
            insert_map(
                &mut out,
                "release_subscriptions:v1",
                decode(rows.get("release_subscriptions:v1"), json!({}))?,
            )?;
            if let Some(raw) = rows.get("calendar_export:v1") {
                out.records.insert(
                    "calendar_export:v1\0value".into(),
                    serde_json::from_str(raw).map_err(|e| e.to_string())?,
                );
            }
        }
        "search" => {
            for key in ["discover:search_history:v2", "discover:search_history"] {
                if let Some(raw) = rows.get(key) {
                    out.records.insert(
                        format!("{key}\0value"),
                        serde_json::from_str(raw).map_err(|e| e.to_string())?,
                    );
                }
            }
        }
        _ => return Err("Unsupported backup component".into()),
    }
    Ok(out)
}

pub(super) fn validate(name: &str, payload: &Payload) -> Result<(), String> {
    if !supported(name) {
        return Err("Unsupported backup component".into());
    }
    for (key, value) in &payload.records {
        let (logical, id) = split(key)?;
        let valid = if let Some(mapped) = map_key(name) {
            logical == mapped
        } else {
            match name {
                "settings" => {
                    (logical == "settings" && portable_setting(id))
                        || matches!(logical, "home:showcase:v1" | "home:catalog-rows:v1")
                }
                "device" => logical == "settings" && DEVICE_FIELDS.contains(&id),
                "preferences" => matches!(
                    logical,
                    "playback_preferences:v1" | "stream_family_preferences:v1"
                ),
                "library" => logical == "library",
                "categories" => {
                    logical == "categories"
                        && value.as_str() == Some(id)
                        && id.trim() == id
                        && !id.is_empty()
                }
                "collections" => logical == "smart_collections",
                "bookmarks" => logical == "scene_bookmarks:v1",
                "journal" => {
                    logical.starts_with("viewing_journal:v1:")
                        && app::viewing_journal::validate_session(logical, &value.to_string())
                            .is_ok()
                }
                "queue" => {
                    logical == "queue_items"
                        || matches!(logical, "queue_settings" | "queue_order" | "queue_cursor")
                            && id == "value"
                }
                "policy" => {
                    logical == app::profile_policy::KEY
                        && id == "value"
                        && serde_json::from_value::<app::profile_policy::ProfilePolicy>(
                            value.clone(),
                        )
                        .is_ok_and(|p| p.validate().is_ok())
                }
                "addons" => matches!(
                    logical,
                    "addons" | "addon_order" | "plugin_settings" | "addon_packages:v1"
                ),
                "packages" => {
                    logical == "package_files"
                        && value.get("id").and_then(Value::as_str) == Some(id)
                        && value
                            .get("sha256")
                            .and_then(Value::as_str)
                            .is_some_and(|hash| {
                                hash.len() == 64
                                    && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                            })
                }
                "integrations" => {
                    logical == "settings" && id == "tmdb_api_key"
                        || logical
                            .strip_prefix('@')
                            .is_some_and(|domain| integration_record(domain, id))
                }
                "reminders" => matches!(logical, "release_subscriptions:v1" | "calendar_export:v1"),
                "search" => matches!(
                    logical,
                    "discover:search_history:v2" | "discover:search_history"
                ),
                _ => false,
            }
        };
        if !valid {
            return Err(format!("Invalid {name} component record"));
        }
    }
    Ok(())
}

pub(super) fn merge(
    name: &str,
    local: &Payload,
    backup: &Payload,
    replace: bool,
    use_backup: bool,
) -> Result<Payload, String> {
    validate(name, backup)?;
    if replace || local.records.is_empty() {
        return Ok(backup.clone());
    }
    if name == "queue" {
        let raw = app::watch_queue::portable_merge(
            &queue_snapshot(local).to_string(),
            &queue_snapshot(backup).to_string(),
            use_backup,
        )?;
        return queue_payload(serde_json::from_str(&raw).map_err(|e| e.to_string())?);
    }
    let mut result = local.clone();
    for (key, value) in &backup.records {
        if name == "policy"
            && let Some(old) = result.records.get(key)
        {
            let local = serde_json::from_value(old.clone()).map_err(|e| e.to_string())?;
            let archived = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            let merged = if use_backup {
                app::profile_policy::merge(&local, &archived, false)
            } else {
                local
            };
            result.records.insert(
                key.clone(),
                serde_json::to_value(merged).map_err(|e| e.to_string())?,
            );
        } else if name == "addons" && key == "addon_order\0value" {
            let mut order = result
                .records
                .get(key)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for item in value.as_array().ok_or("Invalid addon order")? {
                if !order.contains(item) {
                    order.push(item.clone());
                }
            }
            result.records.insert(key.clone(), json!(order));
        } else if name == "library"
            && !use_backup
            && let Some(old) = result.records.get_mut(key)
        {
            if let (Some(local), Some(archived)) = (
                old.get_mut("categories").and_then(Value::as_array_mut),
                value.get("categories").and_then(Value::as_array),
            ) {
                for category in archived {
                    if !local.contains(category) {
                        local.push(category.clone());
                    }
                }
            }
        } else if use_backup || !result.records.contains_key(key) {
            result.records.insert(key.clone(), value.clone());
        }
    }
    Ok(result)
}

pub(super) fn normalize(name: &str, payload: Payload) -> Result<Payload, String> {
    if name == "queue" {
        let empty = json!({"version":2,"entries":[],"order":[],"cursor":{"item_key":null,"last_completed_key":null,"phase":"ready","origin_device":0},"autoplay":false});
        let raw = app::watch_queue::portable_merge(
            &empty.to_string(),
            &queue_snapshot(&payload).to_string(),
            true,
        )?;
        return queue_payload(serde_json::from_str(&raw).map_err(|e| e.to_string())?);
    }
    if name == "preferences" {
        let mut payload = payload;
        let families = payload
            .records
            .iter()
            .filter_map(|(key, value)| {
                key.strip_prefix("stream_family_preferences:v1\0")
                    .map(|id| (id.to_owned(), value.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        let normalized = app::last_stream::portable_families(&json!(families).to_string())?;
        payload
            .records
            .retain(|key, _| !key.starts_with("stream_family_preferences:v1\0"));
        insert_map(
            &mut payload,
            app::last_stream::FAMILY_KEY,
            serde_json::from_str(&normalized).map_err(|e| e.to_string())?,
        )?;
        return Ok(payload);
    }
    Ok(payload)
}

/// Materialize only the selector's owned fields; all other settings survive.
pub(super) fn materialize(
    name: &str,
    payload: &Payload,
    before: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, Option<String>>, String> {
    validate(name, payload)?;
    if name == "packages" {
        return Ok(BTreeMap::new());
    }
    let mut groups: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    for (key, value) in &payload.records {
        let (logical, id) = split(key)?;
        groups
            .entry(logical.into())
            .or_default()
            .insert(id.into(), value.clone());
    }
    let mut edits = BTreeMap::new();
    if matches!(name, "settings" | "device" | "categories" | "collections") {
        let mut settings = decode(
            before.get("settings"),
            serde_json::to_value(nova_config::CacheSettings::default()).unwrap(),
        )?;
        let map = settings.as_object_mut().ok_or("Invalid settings")?;
        match name {
            "categories" => {
                map.insert(
                    "categories".into(),
                    json!(
                        groups
                            .remove("categories")
                            .unwrap_or_default()
                            .into_values()
                            .collect::<Vec<_>>()
                    ),
                );
            }
            "collections" => {
                map.insert(
                    "smart_collections".into(),
                    json!(
                        groups
                            .remove("smart_collections")
                            .unwrap_or_default()
                            .into_values()
                            .collect::<Vec<_>>()
                    ),
                );
            }
            _ => {
                let defaults = serde_json::to_value(nova_config::CacheSettings::default()).unwrap();
                // Components are complete exports. A missing known field resets
                // to its default only within this explicitly selected component.
                let owned = |key: &str| {
                    if name == "settings" {
                        portable_setting(key)
                    } else {
                        DEVICE_FIELDS.contains(&key)
                    }
                };
                for (key, value) in defaults.as_object().unwrap() {
                    if owned(key) {
                        map.insert(key.clone(), value.clone());
                    }
                }
                map.extend(groups.remove("settings").unwrap_or_default());
            }
        }
        edits.insert("settings".into(), Some(settings.to_string()));
    }
    if let Some(key) = map_key(name) {
        edits.insert(
            key.into(),
            Some(
                serde_json::to_string(&groups.remove(key).unwrap_or_default())
                    .map_err(|e| e.to_string())?,
            ),
        );
    }
    match name {
        "integrations" => {
            let mut settings = decode(
                before.get("settings"),
                serde_json::to_value(nova_config::CacheSettings::default()).unwrap(),
            )?;
            let value = groups
                .remove("settings")
                .and_then(|mut values| values.remove("tmdb_api_key"))
                .unwrap_or(json!(""));
            settings
                .as_object_mut()
                .ok_or("Invalid settings")?
                .insert("tmdb_api_key".into(), value);
            edits.insert("settings".into(), Some(settings.to_string()));
        }
        "preferences" => {
            for key in ["playback_preferences:v1", app::last_stream::FAMILY_KEY] {
                let raw = json!(groups.remove(key).unwrap_or_default()).to_string();
                let raw = if key == app::last_stream::FAMILY_KEY {
                    app::last_stream::portable_families(&raw)?
                } else {
                    raw
                };
                edits.insert(key.into(), Some(raw));
            }
        }
        "library" => {
            edits.insert(
                "library".into(),
                Some(
                    json!(
                        groups
                            .remove("library")
                            .unwrap_or_default()
                            .into_values()
                            .collect::<Vec<_>>()
                    )
                    .to_string(),
                ),
            );
        }
        "bookmarks" => {
            let records = groups
                .remove("scene_bookmarks:v1")
                .unwrap_or_default()
                .into_iter()
                .map(|(id, value)| (id, value.to_string()))
                .collect::<Vec<_>>();
            edits.insert(
                "scene_bookmarks:v1".into(),
                Some(app::activity_sync::bookmark_snapshot(&records).map_err(|e| e.to_string())?),
            );
        }
        "addons" => {
            let order = groups
                .remove("addon_order")
                .and_then(|g| g.get("value").cloned())
                .unwrap_or(json!([]));
            let mut addons = groups
                .remove("addons")
                .unwrap_or_default()
                .into_values()
                .collect::<Vec<_>>();
            addons.sort_by_key(|value| {
                order
                    .as_array()
                    .and_then(|rows| rows.iter().position(|v| v == &value["url"]))
                    .unwrap_or(usize::MAX)
            });
            edits.insert("addons".into(), Some(json!(addons).to_string()));
            for key in ["plugin_settings", "addon_packages:v1"] {
                edits.insert(
                    key.into(),
                    Some(json!(groups.remove(key).unwrap_or_default()).to_string()),
                );
            }
        }
        "journal" => {
            let selected: BTreeSet<_> = groups.keys().cloned().collect();
            for key in before
                .keys()
                .filter(|key| key.starts_with("viewing_journal:v1:") && !selected.contains(*key))
            {
                edits.insert(key.clone(), None);
            }
        }
        "reminders" => {
            edits.insert(
                "release_subscriptions:v1".into(),
                Some(
                    json!(
                        groups
                            .remove("release_subscriptions:v1")
                            .unwrap_or_default()
                    )
                    .to_string(),
                ),
            );
            if !groups.contains_key("calendar_export:v1") {
                edits.insert("calendar_export:v1".into(), None);
            }
        }
        "search" => {
            for key in ["discover:search_history:v2", "discover:search_history"] {
                if !groups.contains_key(key) {
                    edits.insert(key.into(), None);
                }
            }
        }
        "queue" => {
            app::watch_queue::records(&queue_snapshot(payload).to_string())
                .map_err(|e| e.to_string())?;
            edits.insert(
                app::watch_queue::KEY.into(),
                Some(queue_snapshot(payload).to_string()),
            );
            groups.retain(|key, _| !key.starts_with("queue_"));
        }
        "policy" => {
            edits.insert(
                app::profile_policy::KEY.into(),
                Some(
                    groups
                        .remove(app::profile_policy::KEY)
                        .and_then(|mut g| g.remove("value"))
                        .unwrap_or(
                            serde_json::to_value(app::profile_policy::ProfilePolicy::default())
                                .unwrap(),
                        )
                        .to_string(),
                ),
            );
        }
        _ => {}
    }
    for (logical, values) in groups {
        if logical.starts_with('@') {
            continue;
        }
        if let Some(value) = values.get("value").or_else(|| values.values().next()) {
            edits.insert(logical, Some(value.to_string()));
        }
    }
    Ok(edits)
}

type IntegrationDomains = BTreeMap<String, Vec<(String, String, u64)>>;

pub(super) fn integration_domains(payload: &Payload) -> Result<IntegrationDomains, String> {
    let mut domains = IntegrationDomains::new();
    for (key, value) in &payload.records {
        let (domain, key) = split(key)?;
        if !domain.starts_with('@') {
            continue;
        }
        domains
            .entry(domain.trim_start_matches('@').into())
            .or_default()
            .push((key.into(), value.to_string(), 0));
    }
    for domain in ["tracking", "stremio", "list_services"] {
        domains.entry(domain.into()).or_default();
    }
    Ok(domains)
}

/// Rebuild service materialization from portable configuration, never an
/// archived delivery outbox or a previous target's stale pending deletion.
pub(super) fn integration_local_edits(
    payload: &Payload,
    before: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, Option<String>>, String> {
    let domains = integration_domains(payload)?;
    let records = domains
        .get("tracking")
        .unwrap()
        .iter()
        .map(|(key, value, _)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    let sequence = before
        .get(nova_tracking::EVENT_COUNTER_KEY)
        .map(|raw| serde_json::from_str::<u64>(raw))
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or(0);
    let mut state = nova_tracking::peer::apply_peer_records(
        &nova_tracking::TrackingState::default(),
        &records,
        sequence,
        |_| false,
    )
    .map_err(|e| e.to_string())?;
    state.outbox = nova_tracking::Outbox::default();
    state.completion.pending.clear();
    let mut edits = BTreeMap::from([(
        nova_tracking::TRACKING_STATE_KEY.into(),
        Some(json!({"version":5,"state":state}).to_string()),
    )]);
    for (shared, local) in [
        ("credentials:mal", "tracking:credentials:mal:v1"),
        ("credentials:anilist", "tracking:credentials:anilist:v1"),
    ] {
        edits.insert(
            local.into(),
            records
                .iter()
                .find(|(key, _)| key == shared)
                .map(|(_, raw)| raw.clone()),
        );
    }
    for key in before.keys().filter(|key| {
        key.starts_with("stremio:state:")
            || key.starts_with("list_services:simkl_cache:")
            || key.starts_with("list_services:boundary:")
    }) {
        edits.insert(key.clone(), None);
    }
    Ok(edits)
}

//! Plugin-scoped durable preferences and independent bounded TTL/LRU cache.
//! Storage is injected by the app so preferences can share its sync transaction.
use crate::ProviderHostError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex, OnceLock},
};

pub const SETTINGS_LIMIT: usize = 1024 * 1024;
const VALUE_LIMIT: usize = 16 * 1024;
const CACHE_LIMIT: usize = 20 * 1024 * 1024;
const CACHE_ENTRY_LIMIT: usize = 1024 * 1024;
const CACHE_COUNT_LIMIT: usize = 1024;
pub type SettingsMap = BTreeMap<String, Map<String, Value>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettingOption {
    pub value: Value,
    pub label: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettingField {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub default: Value,
    #[serde(default)]
    pub options: Vec<SettingOption>,
    pub min: Option<i64>,
    pub max: Option<i64>,
}
impl SettingField {
    pub fn validate(&self, value: &Value) -> Result<(), ProviderHostError> {
        validate_key(&self.key)?;
        let valid = match self.kind.as_str() {
            "boolean" => value.is_boolean(),
            "string" | "secret" => value.is_string(),
            "integer" => value.as_i64().is_some_and(|n| {
                self.min.is_none_or(|min| n >= min) && self.max.is_none_or(|max| n <= max)
            }),
            "choice" => self.options.iter().any(|o| o.value == *value),
            _ => false,
        };
        if !valid {
            return Err(error("invalid setting value"));
        }
        validate_value(value)
    }
}
#[derive(Clone, Debug)]
pub struct SettingsSnapshot {
    /// Declared schema fields only; unknown durable values remain in storage.
    pub values: Map<String, Value>,
    pub generation: u64,
}

pub trait PluginSettingsBackend: Send + Sync {
    fn load_settings(&self) -> Result<Option<String>, ProviderHostError>;
    fn save_settings(&self, raw: &str) -> Result<(), ProviderHostError>;
    fn load_cache(&self) -> Result<Option<String>, ProviderHostError>;
    fn save_cache(&self, raw: &str) -> Result<(), ProviderHostError>;
}
#[derive(Clone, Serialize, Deserialize)]
struct CacheEntry {
    plugin: String,
    key: String,
    value: String,
    expires: u64,
    used: u64,
    #[serde(default)]
    settings_hash: u64,
}
#[derive(Default)]
struct Service {
    backend: Option<Arc<dyn PluginSettingsBackend>>,
    settings: SettingsMap,
    generation: u64,
    cache: BTreeMap<String, CacheEntry>,
    tick: u64,
}
fn service() -> &'static Mutex<Service> {
    static STORE: OnceLock<Mutex<Service>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Service::default()))
}
fn error(message: &str) -> ProviderHostError {
    ProviderHostError(message.into())
}
pub fn validate_plugin(plugin: &str) -> Result<(), ProviderHostError> {
    if !plugin
        .bytes()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        || plugin.len() > 128
        || !plugin
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_- .".contains(&c))
        || plugin.contains(' ')
    {
        return Err(error("invalid plugin id"));
    }
    Ok(())
}
pub fn validate_key(key: &str) -> Result<(), ProviderHostError> {
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-.:".contains(&c))
    {
        return Err(error("invalid plugin key"));
    }
    Ok(())
}
fn validate_value(value: &Value) -> Result<(), ProviderHostError> {
    if value.to_string().len() > VALUE_LIMIT {
        Err(error("setting exceeds size limit"))
    } else {
        Ok(())
    }
}
pub fn parse_settings(raw: &str) -> Result<SettingsMap, ProviderHostError> {
    if raw.len() > SETTINGS_LIMIT {
        return Err(error("settings exceed size limit"));
    }
    let values: SettingsMap =
        serde_json::from_str(raw).map_err(|_| error("unreadable plugin settings"))?;
    for (plugin, fields) in &values {
        validate_plugin(plugin)?;
        if fields.len() > 128 {
            return Err(error("too many plugin settings"));
        }
        for (key, value) in fields {
            validate_key(key)?;
            validate_value(value)?;
        }
    }
    Ok(values)
}
pub fn configure_plugin_settings(
    backend: Arc<dyn PluginSettingsBackend>,
) -> Result<(), ProviderHostError> {
    let settings = backend
        .load_settings()?
        .map(|raw| parse_settings(&raw))
        .transpose()?
        .unwrap_or_default();
    let cache = backend
        .load_cache()?
        .filter(|raw| raw.len() <= CACHE_LIMIT * 6 + 1024 * 1024)
        .and_then(|raw| serde_json::from_str::<BTreeMap<String, CacheEntry>>(&raw).ok())
        .unwrap_or_default();
    let mut state = service().lock().unwrap();
    let generation = state.generation.wrapping_add(1);
    let tick = cache
        .values()
        .map(|entry| entry.used)
        .max()
        .unwrap_or_default();
    *state = Service {
        backend: Some(backend),
        settings,
        generation,
        cache,
        tick,
    };
    prune_cache(&mut state, 0);
    Ok(())
}
pub fn settings_snapshot(
    plugin: &str,
    schema: &[SettingField],
) -> Result<SettingsSnapshot, ProviderHostError> {
    validate_plugin(plugin)?;
    let state = service().lock().unwrap();
    let stored = state.settings.get(plugin);
    // Retain future fields durably, but only declared fields belong to this
    // version's operation. Unknown values must not consume its runtime budget.
    let mut values = Map::new();
    for field in schema {
        field.validate(&field.default)?;
        let value = stored
            .and_then(|values| values.get(&field.key))
            .filter(|value| field.validate(value).is_ok())
            .unwrap_or(&field.default);
        values.insert(field.key.clone(), value.clone());
    }
    Ok(SettingsSnapshot {
        values,
        generation: state.generation,
    })
}
fn update_settings(
    plugin: &str,
    key: Option<&str>,
    value: Option<Value>,
) -> Result<(), ProviderHostError> {
    validate_plugin(plugin)?;
    if let Some(key) = key {
        validate_key(key)?;
    }
    if let Some(value) = &value {
        validate_value(value)?;
    }
    let mut state = service().lock().unwrap();
    let mut next = state.settings.clone();
    match (key, value) {
        (Some(key), Some(value)) => {
            next.entry(plugin.into())
                .or_default()
                .insert(key.into(), value);
        }
        (Some(key), None) => {
            if let Some(fields) = next.get_mut(plugin) {
                fields.remove(key);
            }
        }
        (None, _) => {
            next.remove(plugin);
        }
    }
    next.retain(|_, fields| !fields.is_empty());
    let raw = serde_json::to_string(&next).map_err(|_| error("invalid plugin settings"))?;
    parse_settings(&raw)?;
    if next == state.settings {
        return Ok(());
    }
    let backend = state
        .backend
        .as_ref()
        .ok_or_else(|| error("plugin settings unavailable"))?;
    backend.save_settings(&raw)?;
    state.settings = next;
    state.generation = state.generation.wrapping_add(1);
    // Cached results may contain credential-dependent URLs; invalidate at edit.
    state.cache.retain(|_, entry| entry.plugin != plugin);
    Ok(())
}
pub fn set_setting(plugin: &str, key: &str, value: Value) -> Result<(), ProviderHostError> {
    update_settings(plugin, Some(key), Some(value))
}
pub fn remove_setting(plugin: &str, key: &str) -> Result<(), ProviderHostError> {
    update_settings(plugin, Some(key), None)
}
pub fn clear_settings(plugin: &str) -> Result<(), ProviderHostError> {
    update_settings(plugin, None, None)
}
/// Serialize projection construction, durable storage, and publication with local
/// writes. The loader runs under the service lock so it must read the latest
/// authoritative sync records, and must not call another service operation.
pub fn apply_settings_projection(
    load: impl FnOnce() -> Result<String, ProviderHostError>,
) -> Result<(), ProviderHostError> {
    let mut state = service().lock().unwrap();
    let raw = load()?;
    let next = parse_settings(&raw)?;
    state
        .backend
        .as_ref()
        .ok_or_else(|| error("plugin settings unavailable"))?
        .save_settings(&raw)?;
    if next != state.settings {
        state.settings = next;
        state.generation = state.generation.wrapping_add(1);
        state.cache.clear();
    }
    Ok(())
}
fn cache_key(plugin: &str, key: &str) -> Result<String, ProviderHostError> {
    validate_plugin(plugin)?;
    validate_key(key)?;
    Ok(format!("{plugin}/{key}"))
}
fn settings_hash(state: &Service, plugin: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    state
        .settings
        .get(plugin)
        .map(|values| serde_json::to_string(values).unwrap_or_default())
        .hash(&mut hasher);
    hasher.finish()
}
fn prune_cache(state: &mut Service, now: u64) {
    state.cache.retain(|_, entry| {
        entry.expires > now
            && entry.value.len() <= CACHE_ENTRY_LIMIT
            && validate_plugin(&entry.plugin).is_ok()
            && validate_key(&entry.key).is_ok()
    });
    while state.cache.len() > CACHE_COUNT_LIMIT
        || state
            .cache
            .values()
            .map(|e| e.plugin.len() + e.key.len() + e.value.len())
            .sum::<usize>()
            > CACHE_LIMIT
    {
        let Some(key) = state
            .cache
            .iter()
            .min_by_key(|(_, e)| e.used)
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        state.cache.remove(&key);
    }
}
pub fn cache_get(plugin: &str, key: &str, now: u64) -> Result<Option<String>, ProviderHostError> {
    cache_get_inner(plugin, key, now, None)
}
pub fn cache_get_for_generation(
    plugin: &str,
    key: &str,
    now: u64,
    expected_generation: u64,
) -> Result<Option<String>, ProviderHostError> {
    cache_get_inner(plugin, key, now, Some(expected_generation))
}
fn cache_get_inner(
    plugin: &str,
    key: &str,
    now: u64,
    expected_generation: Option<u64>,
) -> Result<Option<String>, ProviderHostError> {
    let key = cache_key(plugin, key)?;
    let mut state = service().lock().unwrap();
    if expected_generation.is_some_and(|generation| generation != state.generation) {
        return Err(error("plugin settings changed during operation"));
    }
    prune_cache(&mut state, now);
    state.tick = state.tick.wrapping_add(1);
    let tick = state.tick;
    let hash = settings_hash(&state, plugin);
    if state
        .cache
        .get(&key)
        .is_some_and(|entry| entry.settings_hash != hash)
    {
        state.cache.remove(&key);
    }
    Ok(state.cache.get_mut(&key).map(|entry| {
        entry.used = tick;
        entry.value.clone()
    }))
}
pub fn cache_set(
    plugin: &str,
    key: &str,
    value: &str,
    ttl: u64,
    now: u64,
) -> Result<(), ProviderHostError> {
    cache_set_inner(plugin, key, value, ttl, now, None)
}
/// Reject results from a request that began before a configuration edit.
/// Compare while holding the mutation lock, so stale results cannot be tagged
/// with the new settings hash between the generation check and cache write.
pub fn cache_set_for_generation(
    plugin: &str,
    key: &str,
    value: &str,
    ttl: u64,
    now: u64,
    expected_generation: u64,
) -> Result<(), ProviderHostError> {
    cache_set_inner(plugin, key, value, ttl, now, Some(expected_generation))
}
fn cache_set_inner(
    plugin: &str,
    key: &str,
    value: &str,
    ttl: u64,
    now: u64,
    expected_generation: Option<u64>,
) -> Result<(), ProviderHostError> {
    let id = cache_key(plugin, key)?;
    if value.len() > CACHE_ENTRY_LIMIT || ttl > 30 * 86400 {
        return Err(error("cache entry exceeds limits"));
    }
    let mut state = service().lock().unwrap();
    if expected_generation.is_some_and(|generation| generation != state.generation) {
        return Err(error("plugin settings changed during operation"));
    }
    let previous = state.cache.clone();
    state.tick = state.tick.wrapping_add(1);
    let tick = state.tick;
    let hash = settings_hash(&state, plugin);
    state.cache.insert(
        id,
        CacheEntry {
            plugin: plugin.into(),
            key: key.into(),
            value: value.into(),
            expires: now.saturating_add(ttl),
            used: tick,
            settings_hash: hash,
        },
    );
    prune_cache(&mut state, now);
    if let Err(err) = save_cache(&state) {
        state.cache = previous;
        return Err(err);
    }
    Ok(())
}
fn save_cache(state: &Service) -> Result<(), ProviderHostError> {
    let raw = serde_json::to_string(&state.cache).map_err(|_| error("invalid cache"))?;
    state
        .backend
        .as_ref()
        .ok_or_else(|| error("plugin cache unavailable"))?
        .save_cache(&raw)
}
pub fn cache_clear(plugin: &str) -> Result<(), ProviderHostError> {
    validate_plugin(plugin)?;
    let mut state = service().lock().unwrap();
    let previous = state.cache.clone();
    state.cache.retain(|_, entry| entry.plugin != plugin);
    if let Err(err) = save_cache(&state) {
        state.cache = previous;
        return Err(err);
    }
    // Even an empty clear must cancel requests that could repopulate it after
    // the user cleared it. Publish the epoch only after durable success.
    state.generation = state.generation.wrapping_add(1);
    Ok(())
}
pub fn cache_remove(plugin: &str, key: &str) -> Result<(), ProviderHostError> {
    cache_remove_inner(plugin, key, None)
}
/// An obsolete request must not remove a cache entry written by a newer one.
pub fn cache_remove_for_generation(
    plugin: &str,
    key: &str,
    expected_generation: u64,
) -> Result<(), ProviderHostError> {
    cache_remove_inner(plugin, key, Some(expected_generation))
}
fn cache_remove_inner(
    plugin: &str,
    key: &str,
    expected_generation: Option<u64>,
) -> Result<(), ProviderHostError> {
    let key = cache_key(plugin, key)?;
    let mut state = service().lock().unwrap();
    if expected_generation.is_some_and(|generation| generation != state.generation) {
        return Err(error("plugin settings changed during operation"));
    }
    let previous = state.cache.clone();
    state.cache.remove(&key);
    if let Err(err) = save_cache(&state) {
        state.cache = previous;
        return Err(err);
    }
    Ok(())
}

/// Monotonic process generation for rejecting settings-dependent worker results.
pub fn settings_generation() -> u64 {
    service().lock().unwrap().generation
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Memory {
        settings: Mutex<Option<String>>,
        cache: Mutex<Option<String>>,
        fail: std::sync::atomic::AtomicBool,
    }
    impl PluginSettingsBackend for Memory {
        fn load_settings(&self) -> Result<Option<String>, ProviderHostError> {
            Ok(self.settings.lock().unwrap().clone())
        }
        fn save_settings(&self, raw: &str) -> Result<(), ProviderHostError> {
            if self.fail.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(error("disk full"));
            }
            *self.settings.lock().unwrap() = Some(raw.into());
            Ok(())
        }
        fn load_cache(&self) -> Result<Option<String>, ProviderHostError> {
            Ok(self.cache.lock().unwrap().clone())
        }
        fn save_cache(&self, raw: &str) -> Result<(), ProviderHostError> {
            if self.fail.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(error("disk full"));
            }
            *self.cache.lock().unwrap() = Some(raw.into());
            Ok(())
        }
    }
    #[test]
    fn settings_cache_roundtrip_bounds_defaults_and_failed_writes() {
        let backend = Arc::new(Memory::default());
        *backend.settings.lock().unwrap() = Some(r#"{"test":{"future":{"x":1}}}"#.into());
        configure_plugin_settings(backend.clone()).unwrap();
        let fields = vec![SettingField {
            key: "enabled".into(),
            label: "Enabled".into(),
            kind: "boolean".into(),
            default: Value::Bool(false),
            options: vec![],
            min: None,
            max: None,
        }];
        let initial = settings_snapshot("test", &fields).unwrap();
        assert_eq!(initial.values["enabled"], false);
        assert!(!initial.values.contains_key("future"));
        assert_eq!(
            parse_settings(backend.settings.lock().unwrap().as_ref().unwrap()).unwrap()["test"]["future"],
            serde_json::json!({"x":1})
        );
        assert!(fields[0].validate(&Value::String("false".into())).is_err());
        set_setting("test", "enabled", Value::Bool(true)).unwrap();
        assert!(settings_snapshot("test", &fields).unwrap().generation > initial.generation);
        backend
            .fail
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let generation = settings_generation();
        assert!(set_setting("test", "enabled", Value::Bool(false)).is_err());
        assert_eq!(settings_generation(), generation);
        assert!(apply_settings_projection(|| Ok(r#"{"test":{"enabled":false}}"#.into())).is_err());
        assert_eq!(settings_generation(), generation);
        assert!(
            cache_set_for_generation("test", "stale", "stale result", 10, 100, initial.generation)
                .is_err()
        );
        assert!(cache_get("test", "stale", 101).unwrap().is_none());
        assert_eq!(
            settings_snapshot("test", &fields).unwrap().values["enabled"],
            true
        );
        backend
            .fail
            .store(false, std::sync::atomic::Ordering::Relaxed);
        cache_set("test", "one", "cached", 10, 100).unwrap();
        assert_eq!(
            cache_get("test", "one", 109).unwrap().as_deref(),
            Some("cached")
        );
        assert!(cache_get("test", "one", 110).unwrap().is_none());
        cache_set("test", "one", "old credentials", 100, 100).unwrap();
        set_setting("test", "enabled", Value::Bool(false)).unwrap();
        configure_plugin_settings(backend.clone()).unwrap();
        assert!(cache_get("test", "one", 101).unwrap().is_none());
        assert!(set_setting("test", "../bad", Value::Null).is_err());
        assert!(cache_set("test", "large", &"x".repeat(CACHE_ENTRY_LIMIT + 1), 1, 0).is_err());
        for n in 0..=CACHE_COUNT_LIMIT {
            cache_set("test", &format!("key{n}"), "x", 100, 100).unwrap();
        }
        assert!(cache_get("test", "key0", 101).unwrap().is_none());
        assert!(cache_get("test", "key1024", 101).unwrap().is_some());
        let before_clear = settings_generation();
        backend
            .fail
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(cache_clear("test").is_err());
        assert_eq!(settings_generation(), before_clear);
        assert!(cache_get("test", "key1024", 101).unwrap().is_some());
        backend
            .fail
            .store(false, std::sync::atomic::Ordering::Relaxed);
        cache_clear("test").unwrap();
        assert!(
            cache_set_for_generation("test", "late", "old operation", 10, 101, before_clear)
                .is_err()
        );
        assert!(cache_get("test", "late", 102).unwrap().is_none());
        cache_set("test", "replacement", "new operation", 100, 101).unwrap();
        assert!(cache_remove_for_generation("test", "replacement", before_clear).is_err());
        assert_eq!(
            cache_get("test", "replacement", 102).unwrap().as_deref(),
            Some("new operation")
        );
        cache_remove_for_generation("test", "replacement", settings_generation()).unwrap();
        assert!(cache_get("test", "replacement", 102).unwrap().is_none());
        let empty_generation = settings_generation();
        cache_clear("test").unwrap();
        assert!(settings_generation() > empty_generation);
        assert_eq!(
            parse_settings(backend.settings.lock().unwrap().as_ref().unwrap()).unwrap()["test"]["future"],
            serde_json::json!({"x":1})
        );
        // A newer version may retain a full set of large future fields. Its
        // valid durable payload must not prevent an older schema from running.
        let large_unknown = (0..128)
            .map(|i| (format!("future{i}"), Value::String("x".repeat(4096))))
            .collect::<Map<_, _>>();
        let raw = serde_json::to_string(&SettingsMap::from([("capacity".into(), large_unknown)]))
            .unwrap();
        assert!(raw.len() > 256 * 1024);
        assert!(parse_settings(&raw).is_ok());
        *backend.settings.lock().unwrap() = Some(raw.clone());
        configure_plugin_settings(backend.clone()).unwrap();
        let declared = (0..8)
            .map(|i| SettingField {
                key: format!("current{i}"),
                label: format!("Current {i}"),
                kind: "boolean".into(),
                default: Value::Bool(i % 2 == 0),
                options: vec![],
                min: None,
                max: None,
            })
            .collect::<Vec<_>>();
        let snapshot = settings_snapshot("capacity", &declared).unwrap();
        assert_eq!(snapshot.values.len(), 8);
        assert!(serde_json::to_string(&snapshot.values).unwrap().len() < 256 * 1024);
        for field in &declared {
            assert_eq!(snapshot.values[&field.key], field.default);
        }
        assert_eq!(
            backend.settings.lock().unwrap().as_deref(),
            Some(raw.as_str())
        );
        set_setting(
            "capacity",
            "future0",
            Value::String("still retained".into()),
        )
        .unwrap();
        let preserved = parse_settings(backend.settings.lock().unwrap().as_ref().unwrap()).unwrap();
        assert_eq!(preserved["capacity"].len(), 128);
        assert_eq!(
            preserved["capacity"]["future127"],
            Value::String("x".repeat(4096))
        );
        clear_settings("capacity").unwrap();
        assert!(
            !parse_settings(backend.settings.lock().unwrap().as_ref().unwrap())
                .unwrap()
                .contains_key("capacity")
        );
        clear_settings("test").unwrap();
        assert!(
            !settings_snapshot("test", &fields)
                .unwrap()
                .values
                .contains_key("future")
        );
        *service().lock().unwrap() = Service::default();
    }
}

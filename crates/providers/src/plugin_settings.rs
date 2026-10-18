//! Plugin-scoped durable preferences and bounded durable/process-only TTL caches.
//! Storage is injected by the app so preferences can share its sync transaction.
use crate::ProviderHostError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    hash::{Hash, Hasher},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
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
#[serde(rename_all = "camelCase")]
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
    /// Optional declarative collection metadata for grouped configuration UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_add_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_controller: Option<String>,
}
impl SettingField {
    pub fn validate(&self, value: &Value) -> Result<(), ProviderHostError> {
        validate_key(&self.key)?;
        validate_collection_metadata(self)?;
        let valid = match self.kind.as_str() {
            "boolean" => value.is_boolean(),
            "string" | "secret" => value.is_string(),
            "integer" => value.as_i64().is_some_and(|n| {
                self.min.is_none_or(|min| n >= min) && self.max.is_none_or(|max| n <= max)
            }),
            "choice" => self.options.iter().any(|o| o.value == *value),
            "sort-order" => self.valid_sort_order(value),
            "ordered-choices" => self.valid_ordered_choices(value),
            _ => false,
        };
        if !valid {
            return Err(error("invalid setting value"));
        }
        validate_value(value)
    }
    fn valid_ordered_choices(&self, value: &Value) -> bool {
        let Some(items) = value.as_array() else {
            return false;
        };
        let mut seen = BTreeSet::new();
        self.options.len() <= 256
            && items.len() <= self.options.len()
            && self.options.iter().all(|option| option.value.is_string())
            && self
                .options
                .iter()
                .map(|option| option.value.as_str())
                .collect::<BTreeSet<_>>()
                .len()
                == self.options.len()
            && items.iter().all(|item| {
                item.as_str().is_some_and(|key| seen.insert(key))
                    && self.options.iter().any(|option| option.value == *item)
            })
    }

    /// Resolve schema growth without losing the saved criterion order/directions.
    fn normalized_value(&self, value: &Value) -> Option<Value> {
        if self.validate(value).is_ok() {
            return Some(value.clone());
        }
        if self.kind != "sort-order" {
            return None;
        }
        let items = value.as_array()?;
        if items.is_empty() {
            return None;
        }
        let mut merged = items.clone();
        for criterion in self.default.as_array()? {
            if !items
                .iter()
                .any(|item| item.get("key") == criterion.get("key"))
            {
                merged.push(criterion.clone());
            }
        }
        let merged = Value::Array(merged);
        self.validate(&merged).ok().map(|()| merged)
    }

    pub fn apply_priority_action(
        &self,
        current: &Value,
        action: &Value,
    ) -> Result<Value, ProviderHostError> {
        if self.kind != "ordered-choices" {
            return Err(error("not an ordered-choices field"));
        }
        self.validate(current)?;
        let object = action
            .as_object()
            .ok_or_else(|| error("invalid priority action"))?;
        if object.len() != 2 {
            return Err(error("invalid priority action"));
        }
        let choice = object
            .get("choice")
            .ok_or_else(|| error("invalid priority choice"))?;
        let mut items = current
            .as_array()
            .cloned()
            .ok_or_else(|| error("invalid priority order"))?;
        let index = items.iter().position(|item| item == choice);
        match (object.get("priorityAction").and_then(Value::as_str), index) {
            (Some("add"), None) => items.push(choice.clone()),
            (Some("add"), Some(_)) => {}
            (Some("remove"), Some(index)) => {
                items.remove(index);
            }
            (Some("up"), Some(index)) if index > 0 => items.swap(index, index - 1),
            (Some("down"), Some(index)) if index + 1 < items.len() => items.swap(index, index + 1),
            (Some("up" | "down"), Some(_)) => {}
            _ => return Err(error("invalid priority action")),
        }
        let value = Value::Array(items);
        self.validate(&value)?;
        Ok(value)
    }
    fn valid_sort_order(&self, value: &Value) -> bool {
        let Some(items) = value.as_array() else {
            return false;
        };
        let keys = self
            .options
            .iter()
            .filter_map(|option| option.value.as_str())
            .collect::<BTreeSet<_>>();
        if self.options.is_empty()
            || self.options.len() > 16
            || keys.len() != self.options.len()
            || items.len() != keys.len()
            || keys.iter().any(|key| validate_key(key).is_err())
        {
            return false;
        }
        let mut seen = BTreeSet::new();
        items.iter().all(|item| {
            let Some(object) = item.as_object() else {
                return false;
            };
            let Some(key) = object.get("key").and_then(Value::as_str) else {
                return false;
            };
            object.len() == 2
                && keys.contains(key)
                && seen.insert(key)
                && matches!(
                    object.get("direction").and_then(Value::as_str),
                    Some("asc" | "desc")
                )
        })
    }

    /// Apply a UI action to a complete value, never storing an action sentinel.
    pub fn apply_sort_action(
        &self,
        current: &Value,
        action: &Value,
    ) -> Result<Value, ProviderHostError> {
        if self.kind != "sort-order" {
            return Err(error("not a sort-order field"));
        }
        self.validate(current)?;
        let object = action
            .as_object()
            .ok_or_else(|| error("invalid sort action"))?;
        if object.len() != 2 {
            return Err(error("invalid sort action"));
        }
        let criterion = object
            .get("criterion")
            .and_then(Value::as_str)
            .ok_or_else(|| error("invalid sort criterion"))?;
        let mut items = current
            .as_array()
            .cloned()
            .ok_or_else(|| error("invalid sort order"))?;
        let index = items
            .iter()
            .position(|item| item["key"].as_str() == Some(criterion))
            .ok_or_else(|| error("unknown sort criterion"))?;
        match object.get("sortAction").and_then(Value::as_str) {
            Some("up") if index > 0 => items.swap(index, index - 1),
            Some("down") if index + 1 < items.len() => items.swap(index, index + 1),
            Some("up" | "down") => {}
            Some("toggle") => {
                let direction = if items[index]["direction"] == "desc" {
                    "asc"
                } else {
                    "desc"
                };
                items[index]["direction"] = Value::String(direction.into());
            }
            _ => return Err(error("invalid sort action")),
        }
        let result = Value::Array(items);
        self.validate(&result)?;
        Ok(result)
    }
}

fn validate_collection_metadata(field: &SettingField) -> Result<(), ProviderHostError> {
    let Some(collection) = field.collection.as_deref() else {
        if field.collection_label.is_some()
            || field.collection_add_label.is_some()
            || field.item.is_some()
            || field.item_role.is_some()
            || field.item_controller.is_some()
        {
            return Err(error("incomplete collection metadata"));
        }
        return Ok(());
    };
    validate_key(collection)?;
    let (Some(label), Some(add_label), Some(item), Some(role)) = (
        field.collection_label.as_deref(),
        field.collection_add_label.as_deref(),
        field.item.as_deref(),
        field.item_role.as_deref(),
    ) else {
        return Err(error("incomplete collection metadata"));
    };
    if label.trim().is_empty()
        || add_label.trim().is_empty()
        || item.trim().is_empty()
        || !matches!(role, "member" | "enabled" | "secret" | "url")
    {
        return Err(error("invalid collection metadata"));
    }
    validate_key(item)?;
    if let Some(controller) = field.item_controller.as_deref() {
        validate_key(controller)?;
    }
    Ok(())
}
#[derive(Clone, Debug)]
pub struct SettingsSnapshot {
    /// Declared schema fields only; unknown durable values remain in storage.
    pub values: Map<String, Value>,
    pub generation: u64,
}

pub trait PluginSettingsBackend: Send + Sync {
    /// Stable durable owner; None keeps standalone/test configuration isolated.
    fn owner_id(&self) -> Option<String> {
        None
    }
    /// None means this owner no longer permits the installed provider.
    fn provider_revision(&self, _url: &str) -> Result<Option<String>, ProviderHostError> {
        Ok(Some(String::new()))
    }
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
    revision: u64,
    revoked: bool,
    provider_revisions: BTreeMap<String, u64>,
    host: Arc<crate::MemoryProviderHost>,
    cache: BTreeMap<String, CacheEntry>,
    // Process-only entries are never serialized or sent to the backend.
    memory_cache: BTreeMap<String, CacheEntry>,
    tick: u64,
    // A digest tracks durable contents without retaining a second full cache.
    persisted_cache: Mutex<Option<[u8; 32]>>,
}
type ServiceOwner = Arc<Mutex<Service>>;
fn active_service() -> &'static Mutex<ServiceOwner> {
    static ACTIVE: OnceLock<Mutex<ServiceOwner>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(Arc::new(Mutex::new(Service::default()))))
}
fn owners() -> &'static Mutex<BTreeMap<String, Weak<Mutex<Service>>>> {
    static OWNERS: OnceLock<Mutex<BTreeMap<String, Weak<Mutex<Service>>>>> = OnceLock::new();
    OWNERS.get_or_init(Mutex::default)
}
thread_local! { static SCOPED_SERVICE: RefCell<Option<ServiceOwner>> = const { RefCell::new(None) }; }
fn service() -> ServiceOwner {
    SCOPED_SERVICE
        .with(|owner| owner.borrow().clone())
        .unwrap_or_else(|| active_service().lock().unwrap().clone())
}
fn next_generation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
fn selected_generation(state: &Service) -> u64 {
    if SCOPED_SERVICE.with(|owner| owner.borrow().is_some()) {
        state.revision
    } else {
        state.generation
    }
}
fn changed(state: &mut Service) {
    let revision = next_generation();
    state.generation = revision;
    state.revision = revision;
}

/// Settings/host authority owned by the originating durable profile. UI
/// activation generations intentionally do not revoke device-wide transfers.
#[derive(Clone)]
pub struct SettingsCapability {
    owner: ServiceOwner,
    revision: u64,
    provider: Option<(String, u64, String)>,
}
impl SettingsCapability {
    pub fn is_current(&self) -> bool {
        let state = self.owner.lock().unwrap();
        if state.revoked || state.revision != self.revision {
            return false;
        }
        let Some((url, revision, durable_revision)) = &self.provider else {
            return true;
        };
        if state.provider_revisions.get(url).copied().unwrap_or(0) != *revision {
            return false;
        }
        let backend = state.backend.clone();
        drop(state);
        backend.is_some_and(|backend| {
            backend
                .provider_revision(url)
                .is_ok_and(|current| current.as_deref() == Some(durable_revision))
        })
    }
    pub fn with<R>(&self, f: impl FnOnce() -> R) -> R {
        struct Restore(Option<ServiceOwner>);
        impl Drop for Restore {
            fn drop(&mut self) {
                SCOPED_SERVICE.with(|owner| *owner.borrow_mut() = self.0.take());
            }
        }
        let previous = SCOPED_SERVICE.with(|owner| owner.replace(Some(self.owner.clone())));
        let _restore = Restore(previous);
        f()
    }
}
pub fn capture_settings_capability() -> SettingsCapability {
    let owner = service();
    let state = owner.lock().unwrap();
    let revision = state.revision;
    drop(state);
    SettingsCapability {
        owner,
        revision,
        provider: None,
    }
}
pub(crate) fn capture_provider_settings(
    url: &str,
    expected_generation: u64,
) -> Result<SettingsCapability, ProviderHostError> {
    let mut capability = capture_settings_capability();
    let state = capability.owner.lock().unwrap();
    if state.revoked || selected_generation(&state) != expected_generation {
        return Err(error("provider settings changed during operation"));
    }
    let revision = state.provider_revisions.get(url).copied().unwrap_or(0);
    let backend = state
        .backend
        .clone()
        .ok_or_else(|| error("plugin settings unavailable"))?;
    drop(state);
    let durable_revision = backend
        .provider_revision(url)?
        .ok_or_else(|| error("provider unavailable for owner"))?;
    capability.provider = Some((url.into(), revision, durable_revision));
    Ok(capability)
}
pub fn revoke_owner(owner_id: &str) {
    let owner = owners()
        .lock()
        .unwrap()
        .get(owner_id)
        .and_then(Weak::upgrade);
    if let Some(owner) = owner {
        let mut state = owner.lock().unwrap();
        state.revoked = true;
        changed(&mut state);
    }
}
pub fn invalidate_owner(owner_id: &str) {
    let owner = owners()
        .lock()
        .unwrap()
        .get(owner_id)
        .and_then(Weak::upgrade);
    if let Some(owner) = owner {
        changed(&mut owner.lock().unwrap());
    }
}
pub fn revoke_provider(owner_id: &str, url: &str) {
    let owner = owners()
        .lock()
        .unwrap()
        .get(owner_id)
        .and_then(Weak::upgrade);
    if let Some(owner) = owner {
        let mut state = owner.lock().unwrap();
        let revision = state.provider_revisions.entry(url.into()).or_default();
        *revision = revision.wrapping_add(1);
    }
}
pub(crate) fn session_host() -> Arc<crate::MemoryProviderHost> {
    service().lock().unwrap().host.clone()
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
pub struct PreparedPluginSettings {
    owner_id: Option<String>,
    observed_owner: Option<(ServiceOwner, u64)>,
    state: Service,
}

/// Read and validate a replacement without changing the active provider owner.
pub fn prepare_plugin_settings(
    backend: Arc<dyn PluginSettingsBackend>,
) -> Result<PreparedPluginSettings, ProviderHostError> {
    let owner_id = backend.owner_id();
    // Observe before loading: a retained resolver may save newer credentials
    // while preparation reads the backend or waits for activation.
    let observed_owner = owner_id
        .as_ref()
        .and_then(|id| owners().lock().unwrap().get(id).and_then(Weak::upgrade))
        .map(|owner| {
            let revision = owner.lock().unwrap().revision;
            (owner, revision)
        });
    let settings = backend
        .load_settings()?
        .map(|raw| parse_settings(&raw))
        .transpose()?
        .unwrap_or_default();
    let loaded_cache = backend.load_cache()?;
    let persisted_cache = loaded_cache.as_deref().map(cache_digest);
    let cache = loaded_cache
        .filter(|raw| raw.len() <= CACHE_LIMIT * 6 + 1024 * 1024)
        .and_then(|raw| serde_json::from_str::<BTreeMap<String, CacheEntry>>(&raw).ok())
        .unwrap_or_default();
    let tick = cache
        .values()
        .map(|entry| entry.used)
        .max()
        .unwrap_or_default();
    let mut state = Service {
        backend: Some(backend),
        settings,
        cache,
        tick,
        persisted_cache: Mutex::new(persisted_cache),
        ..Service::default()
    };
    prune_cache(&mut state, 0);
    Ok(PreparedPluginSettings {
        owner_id,
        observed_owner,
        state,
    })
}

/// Publish only after the app has prepared its incoming profile successfully.
pub fn activate_plugin_settings(prepared: PreparedPluginSettings) {
    let PreparedPluginSettings {
        owner_id,
        observed_owner,
        mut state,
    } = prepared;
    let mut registry = owners().lock().unwrap();
    registry.retain(|_, owner| owner.strong_count() != 0);
    let existing = owner_id
        .as_ref()
        .and_then(|id| registry.get(id))
        .and_then(Weak::upgrade);
    let owner = if let Some(existing) = existing {
        let mut current = existing.lock().unwrap();
        let unchanged_since_preparation =
            observed_owner.as_ref().is_some_and(|(owner, revision)| {
                Arc::ptr_eq(owner, &existing) && *revision == current.revision
            });
        if unchanged_since_preparation && current.settings != state.settings {
            let revoked = current.revoked;
            let provider_revisions = std::mem::take(&mut current.provider_revisions);
            changed(&mut state);
            state.revoked = revoked;
            state.provider_revisions = provider_revisions;
            *current = state;
        } else {
            current.backend = state.backend;
        }
        current.generation = next_generation();
        drop(current);
        existing
    } else {
        changed(&mut state);
        Arc::new(Mutex::new(state))
    };
    if let Some(id) = owner_id {
        registry.insert(id, Arc::downgrade(&owner));
    }
    drop(registry);
    *active_service().lock().unwrap() = owner;
}
pub fn configure_plugin_settings(
    backend: Arc<dyn PluginSettingsBackend>,
) -> Result<(), ProviderHostError> {
    activate_plugin_settings(prepare_plugin_settings(backend)?);
    Ok(())
}
pub fn settings_snapshot(
    plugin: &str,
    schema: &[SettingField],
) -> Result<SettingsSnapshot, ProviderHostError> {
    validate_plugin(plugin)?;
    validate_collection_schema(schema)?;
    let owner = service();
    let state = owner.lock().unwrap();
    let stored = state.settings.get(plugin);
    // Retain future fields durably, but only declared fields belong to this
    // version's operation. Unknown values must not consume its runtime budget.
    let mut values = Map::new();
    for field in schema {
        field.validate(&field.default)?;
        let value = stored
            .and_then(|values| values.get(&field.key))
            .and_then(|value| field.normalized_value(value))
            .unwrap_or_else(|| field.default.clone());
        values.insert(field.key.clone(), value);
    }
    Ok(SettingsSnapshot {
        values,
        generation: selected_generation(&state),
    })
}

/// Check schema declarations without reading or mutating the settings service.
pub(crate) fn validate_schema(schema: &[SettingField]) -> Result<(), ProviderHostError> {
    if schema.len() > 128 {
        return Err(error("provider configuration exceeds field limit"));
    }
    validate_collection_schema(schema)?;
    let mut keys = std::collections::HashSet::new();
    for field in schema {
        if !keys.insert(&field.key) {
            return Err(error("duplicate setting key"));
        }
        field.validate(&field.default)?;
    }
    Ok(())
}

fn validate_collection_schema(schema: &[SettingField]) -> Result<(), ProviderHostError> {
    for field in schema {
        validate_collection_metadata(field)?;
        let Some(collection) = field.collection.as_deref() else {
            continue;
        };
        let role = field.item_role.as_deref().unwrap_or_default();
        let expected_kind = match role {
            "member" | "enabled" => "boolean",
            "secret" => "secret",
            "url" => "string",
            _ => return Err(error("invalid collection item role")),
        };
        if field.kind != expected_kind {
            return Err(error("collection item type does not match its role"));
        }
        if role == "secret"
            && schema
                .iter()
                .filter(|candidate| {
                    candidate.collection == field.collection
                        && candidate.item == field.item
                        && candidate.item_role.as_deref() == Some("secret")
                })
                .count()
                > 1
        {
            return Err(error("collection item may declare only one secret"));
        }
        if matches!(role, "secret" | "url") {
            let controller = field
                .item_controller
                .as_deref()
                .ok_or_else(|| error("collection editor field has no controller"))?;
            if !schema.iter().any(|candidate| {
                candidate.key == controller
                    && candidate.collection.as_deref() == Some(collection)
                    && candidate.item == field.item
                    && candidate.kind == "boolean"
                    && matches!(candidate.item_role.as_deref(), Some("member" | "enabled"))
            }) {
                return Err(error("collection editor controller is missing"));
            }
        }
        if schema.iter().any(|candidate| {
            candidate.collection.as_deref() == Some(collection)
                && (candidate.collection_label != field.collection_label
                    || candidate.collection_add_label != field.collection_add_label)
        }) {
            return Err(error("collection labels must match across fields"));
        }
    }
    Ok(())
}
fn update_settings(
    plugin: &str,
    key: Option<&str>,
    value: Option<Value>,
    expected_generation: Option<u64>,
) -> Result<u64, ProviderHostError> {
    validate_plugin(plugin)?;
    if let Some(key) = key {
        validate_key(key)?;
    }
    if let Some(value) = &value {
        validate_value(value)?;
    }
    let owner = service();
    let mut state = owner.lock().unwrap();
    if expected_generation.is_some_and(|generation| generation != selected_generation(&state)) {
        return Err(error("plugin settings changed during operation"));
    }
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
        return Ok(selected_generation(&state));
    }
    let backend = state
        .backend
        .as_ref()
        .ok_or_else(|| error("plugin settings unavailable"))?;
    backend.save_settings(&raw)?;
    state.settings = next;
    changed(&mut state);
    // Cached results may contain credential-dependent URLs; invalidate at edit.
    state.cache.retain(|_, entry| entry.plugin != plugin);
    state.memory_cache.retain(|_, entry| entry.plugin != plugin);
    Ok(selected_generation(&state))
}
pub fn set_setting(plugin: &str, key: &str, value: Value) -> Result<(), ProviderHostError> {
    update_settings(plugin, Some(key), Some(value), None).map(|_| ())
}
pub fn remove_setting(plugin: &str, key: &str) -> Result<(), ProviderHostError> {
    update_settings(plugin, Some(key), None, None).map(|_| ())
}
pub fn clear_settings(plugin: &str) -> Result<(), ProviderHostError> {
    update_settings(plugin, None, None, None).map(|_| ())
}
/// Validate ownership under the same lock as the backend write. Returning the
/// resulting generation permits sequential edits from one running invocation.
pub(crate) fn set_setting_for_generation(
    plugin: &str,
    key: &str,
    value: Value,
    generation: u64,
) -> Result<u64, ProviderHostError> {
    update_settings(plugin, Some(key), Some(value), Some(generation))
}
pub(crate) fn remove_setting_for_generation(
    plugin: &str,
    key: &str,
    generation: u64,
) -> Result<u64, ProviderHostError> {
    update_settings(plugin, Some(key), None, Some(generation))
}
/// Serialize projection construction, durable storage, and publication with local
/// writes. The loader runs under the service lock so it must read the latest
/// authoritative sync records, and must not call another service operation.
pub fn apply_settings_projection(
    load: impl FnOnce() -> Result<String, ProviderHostError>,
) -> Result<(), ProviderHostError> {
    let owner = service();
    let mut state = owner.lock().unwrap();
    let raw = load()?;
    let next = parse_settings(&raw)?;
    state
        .backend
        .as_ref()
        .ok_or_else(|| error("plugin settings unavailable"))?
        .save_settings(&raw)?;
    if next != state.settings {
        state.settings = next;
        changed(&mut state);
        state.cache.clear();
        state.memory_cache.clear();
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
    prune_entries(&mut state.cache, now);
}
fn prune_entries(cache: &mut BTreeMap<String, CacheEntry>, now: u64) {
    cache.retain(|id, entry| {
        entry.expires > now
            && entry.value.len() <= CACHE_ENTRY_LIMIT
            && validate_plugin(&entry.plugin).is_ok()
            && validate_key(&entry.key).is_ok()
            // The serialized map key is untrusted cache data too. Check it
            // before reads can select another plugin's embedded entry.
            && id == &format!("{}/{}", entry.plugin, entry.key)
    });
    while cache.len() > CACHE_COUNT_LIMIT
        || cache
            .values()
            .map(|e| e.plugin.len() + e.key.len() + e.value.len())
            .sum::<usize>()
            > CACHE_LIMIT
    {
        let Some(key) = cache
            .iter()
            .min_by_key(|(_, e)| e.used)
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        cache.remove(&key);
    }
}
pub fn cache_get(plugin: &str, key: &str, now: u64) -> Result<Option<String>, ProviderHostError> {
    cache_get_inner(plugin, key, now, None, false)
}
pub fn cache_get_for_generation(
    plugin: &str,
    key: &str,
    now: u64,
    expected_generation: u64,
) -> Result<Option<String>, ProviderHostError> {
    cache_get_inner(plugin, key, now, Some(expected_generation), false)
}
fn cache_get_inner(
    plugin: &str,
    key: &str,
    now: u64,
    expected_generation: Option<u64>,
    memory_only: bool,
) -> Result<Option<String>, ProviderHostError> {
    let key = cache_key(plugin, key)?;
    let owner = service();
    let mut state = owner.lock().unwrap();
    if expected_generation.is_some_and(|generation| generation != selected_generation(&state)) {
        return Err(error("plugin settings changed during operation"));
    }
    if memory_only {
        prune_entries(&mut state.memory_cache, now);
    } else {
        prune_cache(&mut state, now);
    }
    state.tick = state.tick.wrapping_add(1);
    let tick = state.tick;
    let hash = settings_hash(&state, plugin);
    let cache = if memory_only {
        &mut state.memory_cache
    } else {
        &mut state.cache
    };
    if cache
        .get(&key)
        .is_some_and(|entry| entry.settings_hash != hash)
    {
        cache.remove(&key);
    }
    Ok(cache.get_mut(&key).map(|entry| {
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
    cache_set_inner(plugin, key, value, ttl, now, (None, false))
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
    cache_set_inner(
        plugin,
        key,
        value,
        ttl,
        now,
        (Some(expected_generation), false),
    )
}
fn cache_set_inner(
    plugin: &str,
    key: &str,
    value: &str,
    ttl: u64,
    now: u64,
    scope: (Option<u64>, bool),
) -> Result<(), ProviderHostError> {
    let (expected_generation, memory_only) = scope;
    let id = cache_key(plugin, key)?;
    if value.len() > CACHE_ENTRY_LIMIT || ttl > 30 * 86400 {
        return Err(error("cache entry exceeds limits"));
    }
    let owner = service();
    let mut state = owner.lock().unwrap();
    if expected_generation.is_some_and(|generation| generation != selected_generation(&state)) {
        return Err(error("plugin settings changed during operation"));
    }
    let previous = (!memory_only).then(|| state.cache.clone());
    state.tick = state.tick.wrapping_add(1);
    let tick = state.tick;
    let hash = settings_hash(&state, plugin);
    let cache = if memory_only {
        &mut state.memory_cache
    } else {
        &mut state.cache
    };
    cache.insert(
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
    prune_entries(cache, now);
    if let Some(previous) = previous
        && let Err(err) = save_cache(&state)
    {
        state.cache = previous;
        return Err(err);
    }
    Ok(())
}
fn cache_digest(raw: &str) -> [u8; 32] {
    aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, raw.as_bytes())
        .as_ref()
        .try_into()
        .unwrap()
}
fn save_cache(state: &Service) -> Result<(), ProviderHostError> {
    let raw = serde_json::to_string(&state.cache).map_err(|_| error("invalid cache"))?;
    let digest = cache_digest(&raw);
    let mut persisted = state.persisted_cache.lock().unwrap();
    if *persisted == Some(digest) {
        return Ok(());
    }
    state
        .backend
        .as_ref()
        .ok_or_else(|| error("plugin cache unavailable"))?
        .save_cache(&raw)?;
    *persisted = Some(digest);
    Ok(())
}
/// Maintenance hook for background GC passes. Persist even if a prior read already
/// removed expired entries in memory: their durable rows still need collection.
/// This leaves preference generations and valid active cache entries untouched.
pub fn cache_gc(now: u64) -> Result<usize, ProviderHostError> {
    let owner = service();
    let mut state = owner.lock().unwrap();
    let previous = state.cache.clone();
    prune_cache(&mut state, now);
    let plugins = state
        .cache
        .values()
        .map(|entry| entry.plugin.clone())
        .collect::<BTreeSet<_>>();
    let hashes = plugins
        .into_iter()
        .map(|plugin| {
            let hash = settings_hash(&state, &plugin);
            (plugin, hash)
        })
        .collect::<BTreeMap<_, _>>();
    state.cache.retain(|id, entry| {
        id == &format!("{}/{}", entry.plugin, entry.key)
            && hashes.get(&entry.plugin) == Some(&entry.settings_hash)
    });
    let removed = previous.len() - state.cache.len();
    if let Err(error) = save_cache(&state) {
        state.cache = previous;
        return Err(error);
    }
    Ok(removed)
}

pub fn cache_clear(plugin: &str) -> Result<(), ProviderHostError> {
    validate_plugin(plugin)?;
    let owner = service();
    let mut state = owner.lock().unwrap();
    let previous = state.cache.clone();
    state.cache.retain(|_, entry| entry.plugin != plugin);
    if let Err(err) = save_cache(&state) {
        state.cache = previous;
        return Err(err);
    }
    state.memory_cache.retain(|_, entry| entry.plugin != plugin);
    // Even an empty clear must cancel requests that could repopulate it after
    // the user cleared it. Publish the epoch only after durable success.
    changed(&mut state);
    Ok(())
}
pub fn cache_remove(plugin: &str, key: &str) -> Result<(), ProviderHostError> {
    cache_remove_inner(plugin, key, None, false)
}
/// An obsolete request must not remove a cache entry written by a newer one.
pub fn cache_remove_for_generation(
    plugin: &str,
    key: &str,
    expected_generation: u64,
) -> Result<(), ProviderHostError> {
    cache_remove_inner(plugin, key, Some(expected_generation), false)
}
fn cache_remove_inner(
    plugin: &str,
    key: &str,
    expected_generation: Option<u64>,
    memory_only: bool,
) -> Result<(), ProviderHostError> {
    let key = cache_key(plugin, key)?;
    let owner = service();
    let mut state = owner.lock().unwrap();
    if expected_generation.is_some_and(|generation| generation != selected_generation(&state)) {
        return Err(error("plugin settings changed during operation"));
    }
    if memory_only {
        state.memory_cache.remove(&key);
        return Ok(());
    }
    let previous = state.cache.clone();
    state.cache.remove(&key);
    if let Err(err) = save_cache(&state) {
        state.cache = previous;
        return Err(err);
    }
    Ok(())
}

/// Process-only cache access uses the same scope, fingerprint, quota and epoch
/// checks as durable cache access, without requiring or writing a backend.
pub fn cache_get_memory_for_generation(
    plugin: &str,
    key: &str,
    now: u64,
    expected_generation: u64,
) -> Result<Option<String>, ProviderHostError> {
    cache_get_inner(plugin, key, now, Some(expected_generation), true)
}
pub fn cache_set_memory_for_generation(
    plugin: &str,
    key: &str,
    value: &str,
    ttl: u64,
    now: u64,
    expected_generation: u64,
) -> Result<(), ProviderHostError> {
    cache_set_inner(
        plugin,
        key,
        value,
        ttl,
        now,
        (Some(expected_generation), true),
    )
}
pub fn cache_remove_memory_for_generation(
    plugin: &str,
    key: &str,
    expected_generation: u64,
) -> Result<(), ProviderHostError> {
    cache_remove_inner(plugin, key, Some(expected_generation), true)
}

/// Monotonic process generation for rejecting settings-dependent worker results.
pub fn settings_generation() -> u64 {
    selected_generation(&service().lock().unwrap())
}

#[cfg(test)]
mod tests {
    #[test]
    fn collection_rejects_multiple_credentials_for_one_item() {
        let schema: Vec<super::SettingField> = serde_json::from_value(serde_json::json!([
            {"key":"account.enabled", "label":"Account", "type":"boolean", "default":false,
             "collection":"accounts", "collectionLabel":"Accounts", "collectionAddLabel":"Add account",
             "item":"account", "itemRole":"enabled"},
            {"key":"account.key", "label":"Key", "type":"secret", "default":"",
             "collection":"accounts", "collectionLabel":"Accounts", "collectionAddLabel":"Add account",
             "item":"account", "itemRole":"secret", "itemController":"account.enabled"},
            {"key":"account.other", "label":"Other key", "type":"secret", "default":"",
             "collection":"accounts", "collectionLabel":"Accounts", "collectionAddLabel":"Add account",
             "item":"account", "itemRole":"secret", "itemController":"account.enabled"}
        ])).unwrap();
        assert!(super::validate_collection_schema(&schema).is_err());
        assert!(super::validate_collection_schema(&schema[..2]).is_ok());
    }

    use super::*;
    use crate::ProviderHost;
    #[derive(Default)]
    struct Memory {
        owner: Option<String>,
        settings: Mutex<Option<String>>,
        cache: Mutex<Option<String>>,
        cache_writes: std::sync::atomic::AtomicUsize,
        fail: std::sync::atomic::AtomicBool,
    }
    impl PluginSettingsBackend for Memory {
        fn owner_id(&self) -> Option<String> {
            self.owner.clone()
        }
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
            self.cache_writes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            collection: None,
            collection_label: None,
            collection_add_label: None,
            item: None,
            item_role: None,
            item_controller: None,
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
        // Memory entries share validation and fingerprints, but do not touch
        // either durable projection, even when the backend cannot write.
        crate::runtime::exercise_memory_cache_runtime();
        let epoch = settings_generation();
        let disk_cache = backend.cache.lock().unwrap().clone();
        backend
            .fail
            .store(true, std::sync::atomic::Ordering::Relaxed);
        cache_set_memory_for_generation("test", "memory", "only in process", 10, 100, epoch)
            .unwrap();
        assert_eq!(
            cache_get_memory_for_generation("test", "memory", 109, epoch)
                .unwrap()
                .as_deref(),
            Some("only in process")
        );
        assert!(
            cache_get_memory_for_generation("other", "memory", 109, epoch)
                .unwrap()
                .is_none()
        );
        assert_eq!(backend.cache.lock().unwrap().clone(), disk_cache);
        assert!(
            cache_get_memory_for_generation("test", "memory", 110, epoch)
                .unwrap()
                .is_none()
        );
        cache_set_memory_for_generation("test", "fingerprint", "stale", 100, 100, epoch).unwrap();
        service()
            .lock()
            .unwrap()
            .memory_cache
            .get_mut("test/fingerprint")
            .unwrap()
            .settings_hash ^= 1;
        assert!(
            cache_get_memory_for_generation("test", "fingerprint", 101, epoch)
                .unwrap()
                .is_none()
        );
        assert!(cache_set_memory_for_generation("test", "../bad", "x", 10, 100, epoch).is_err());
        assert!(
            cache_set_memory_for_generation(
                "test",
                "big",
                &"x".repeat(CACHE_ENTRY_LIMIT + 1),
                10,
                100,
                epoch
            )
            .is_err()
        );
        assert!(
            cache_set_memory_for_generation("test", "ttl", "x", 30 * 86400 + 1, 100, epoch)
                .is_err()
        );
        for n in 0..=CACHE_COUNT_LIMIT {
            cache_set_memory_for_generation("test", &format!("mem{n}"), "x", 100, 100, epoch)
                .unwrap();
        }
        assert!(
            cache_get_memory_for_generation("test", "mem0", 101, epoch)
                .unwrap()
                .is_none()
        );
        assert!(
            cache_get_memory_for_generation("test", "mem1024", 101, epoch)
                .unwrap()
                .is_some()
        );
        for n in 0..21 {
            cache_set_memory_for_generation(
                "test",
                &format!("large{n}"),
                &"x".repeat(CACHE_ENTRY_LIMIT),
                100,
                100,
                epoch,
            )
            .unwrap();
        }
        assert!(
            service()
                .lock()
                .unwrap()
                .memory_cache
                .values()
                .map(|e| e.plugin.len() + e.key.len() + e.value.len())
                .sum::<usize>()
                <= CACHE_LIMIT
        );
        assert!(
            cache_get_memory_for_generation("test", "large0", 101, epoch)
                .unwrap()
                .is_none()
        );
        backend
            .fail
            .store(false, std::sync::atomic::Ordering::Relaxed);
        configure_plugin_settings(backend.clone()).unwrap();
        let epoch = settings_generation();
        assert!(
            cache_get_memory_for_generation("test", "large20", 101, epoch)
                .unwrap()
                .is_none()
        );
        assert_eq!(backend.cache.lock().unwrap().clone(), disk_cache);
        cache_set_memory_for_generation("test", "settings", "old", 100, 100, epoch).unwrap();
        set_setting("test", "enabled", Value::Bool(false)).unwrap();
        let new_epoch = settings_generation();
        assert!(cache_get_memory_for_generation("test", "settings", 101, epoch).is_err());
        assert!(
            cache_set_memory_for_generation("test", "settings", "stale", 100, 100, epoch).is_err()
        );
        assert!(cache_remove_memory_for_generation("test", "settings", epoch).is_err());
        assert!(
            cache_get_memory_for_generation("test", "settings", 101, new_epoch)
                .unwrap()
                .is_none()
        );
        cache_set_memory_for_generation("test", "projection", "old", 100, 100, new_epoch).unwrap();
        apply_settings_projection(|| Ok(r#"{"test":{"enabled":true,"future":{"x":1}}}"#.into()))
            .unwrap();
        assert!(service().lock().unwrap().memory_cache.is_empty());
        cache_set("test", "one", "cached", 10, 100).unwrap();
        let before_restart = settings_generation();
        configure_plugin_settings(backend.clone()).unwrap();
        let after_restart = settings_generation();
        assert_ne!(before_restart, after_restart);
        // A fresh runtime can reuse a persisted entry with unchanged settings;
        // workers from before reconfiguration still cannot publish or read it.
        assert_eq!(
            cache_get_for_generation("test", "one", 109, after_restart)
                .unwrap()
                .as_deref(),
            Some("cached")
        );
        assert!(cache_get_for_generation("test", "one", 109, before_restart).is_err());
        assert!(
            cache_set_for_generation("test", "one", "obsolete", 10, 100, before_restart).is_err()
        );
        assert_eq!(
            cache_get("test", "one", 109).unwrap().as_deref(),
            Some("cached")
        );
        assert!(cache_get("test", "one", 110).unwrap().is_none());
        // Reading expires only the in-memory entry. An explicit maintenance
        // pass must still collect the durable copy even when its count is zero.
        let gc_generation = settings_generation();
        assert_eq!(cache_gc(110).unwrap(), 0);
        let saved: BTreeMap<String, CacheEntry> =
            serde_json::from_str(backend.cache.lock().unwrap().as_ref().unwrap()).unwrap();
        assert!(saved.is_empty());
        let writes = backend
            .cache_writes
            .load(std::sync::atomic::Ordering::Relaxed);
        cache_gc(110).unwrap();
        assert_eq!(
            backend
                .cache_writes
                .load(std::sync::atomic::Ordering::Relaxed),
            writes,
            "an unchanged maintenance pass must not rewrite the cache file"
        );
        cache_set("test", "expired", "expired", 10, 100).unwrap();
        cache_set("test", "valid", "retained", 100, 100).unwrap();
        // A stale fingerprint is collected alongside time-based expiration.
        {
            let owner = service();
            let mut state = owner.lock().unwrap();
            let mut stale = state.cache["test/valid"].clone();
            stale.key = "stale".into();
            stale.settings_hash = stale.settings_hash.wrapping_add(1);
            state.cache.insert("test/stale".into(), stale);
            save_cache(&state).unwrap();
        }
        let disk_before = backend.cache.lock().unwrap().clone();
        backend
            .fail
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(cache_gc(110).is_err());
        assert_eq!(service().lock().unwrap().cache.len(), 3);
        assert_eq!(backend.cache.lock().unwrap().clone(), disk_before);
        assert_eq!(settings_generation(), gc_generation);
        backend
            .fail
            .store(false, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(cache_gc(110).unwrap(), 2);
        let saved: BTreeMap<String, CacheEntry> =
            serde_json::from_str(backend.cache.lock().unwrap().as_ref().unwrap()).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved["test/valid"].value, "retained");
        assert_eq!(
            cache_get("test", "valid", 111).unwrap().as_deref(),
            Some("retained")
        );
        assert_eq!(settings_generation(), gc_generation);
        // Cache files can be edited, corrupted, or evicted independently of
        // preferences. Reject mismatched identities at load, before a GC pass.
        let mut poisoned = saved;
        let mut alias = poisoned["test/valid"].clone();
        alias.value = "wrong entry".into();
        poisoned.insert("test/alias".into(), alias);
        *backend.cache.lock().unwrap() = Some(serde_json::to_string(&poisoned).unwrap());
        configure_plugin_settings(backend.clone()).unwrap();
        assert!(!service().lock().unwrap().cache.contains_key("test/alias"));
        assert!(cache_get("test", "alias", 111).unwrap().is_none());
        assert_eq!(
            cache_get("test", "valid", 111).unwrap().as_deref(),
            Some("retained")
        );
        // Simulate an OS cache eviction followed by a process restart.
        *backend.cache.lock().unwrap() = None;
        configure_plugin_settings(backend.clone()).unwrap();
        assert!(cache_get("test", "valid", 111).unwrap().is_none());
        assert_eq!(
            settings_snapshot("test", &fields).unwrap().values["enabled"],
            true
        );
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
        cache_set_memory_for_generation("test", "clear", "keep on failure", 100, 100, before_clear)
            .unwrap();
        backend
            .fail
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(cache_clear("test").is_err());
        assert_eq!(
            cache_get_memory_for_generation("test", "clear", 101, before_clear)
                .unwrap()
                .as_deref(),
            Some("keep on failure")
        );
        assert_eq!(settings_generation(), before_clear);
        assert!(cache_get("test", "key1024", 101).unwrap().is_some());
        backend
            .fail
            .store(false, std::sync::atomic::Ordering::Relaxed);
        cache_clear("test").unwrap();
        assert!(
            cache_get_memory_for_generation("test", "clear", 101, settings_generation())
                .unwrap()
                .is_none()
        );
        assert!(
            cache_set_memory_for_generation("test", "late", "stale", 100, 101, before_clear)
                .is_err()
        );
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
                collection: None,
                collection_label: None,
                collection_add_label: None,
                item: None,
                item_role: None,
                item_controller: None,
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
        // A profile change swaps the backend even when its values match.
        // In-flight invocations retain their owner and cannot write the new one.
        let replacement = Arc::new(Memory::default());
        let previous_owner = settings_generation();
        configure_plugin_settings(replacement.clone()).unwrap();
        assert!(
            set_setting_for_generation("test", "enabled", Value::Bool(true), previous_owner)
                .is_err()
        );
        assert!(replacement.settings.lock().unwrap().is_none());
        let first =
            set_setting_for_generation("test", "enabled", Value::Bool(true), settings_generation())
                .unwrap();
        let second = set_setting_for_generation("test", "other", Value::Bool(true), first).unwrap();
        remove_setting_for_generation("test", "other", second).unwrap();
        assert_eq!(
            parse_settings(replacement.settings.lock().unwrap().as_ref().unwrap()).unwrap()["test"]
                ["enabled"],
            true
        );
        // Preparation must not publish failed incoming data. Resolver authority
        // then survives another owner's configuration and package registry,
        // but an actual credential/config edit by its own owner still revokes.
        let alice = Arc::new(Memory {
            owner: Some("capability-alice".into()),
            ..Memory::default()
        });
        *alice.settings.lock().unwrap() = Some(r#"{"resolverfixture":{"route":"alice"}}"#.into());
        configure_plugin_settings(alice.clone()).unwrap();
        let package = Arc::new(crate::AddonPackage {
            plugin: serde_json::from_value(serde_json::json!({
                "formatVersion":1,"id":"resolverfixture","name":"Resolver fixture","version":"1.0.0","entry":"index.js","permissions":{},
                "configuration":[{"key":"route","label":"Route","type":"string","default":""}]
            })).unwrap(),
            addon: serde_json::from_value(serde_json::json!({"id":"nova.resolverfixture","name":"Resolver fixture","version":"1.0.0"})).unwrap(),
            source: "globalThis.novaProvider={handle(request){return {id:'fixture',title:'Fixture stream',url:'https://example.test/'+JSON.parse(nova.settings.get('route'))};}};".into(),
            assets: BTreeMap::new(), digest: String::new(),
        });
        let previous_packages = crate::installed_packages();
        let mut packages = previous_packages.clone();
        packages.push(package);
        crate::set_installed_packages(packages.clone());
        let capability = crate::capture_resolver_capability(
            "nova-provider://resolverfixture",
            settings_generation(),
        )
        .unwrap();
        let owner_settings = capture_settings_capability();
        crate::host::session_state()
            .storage_set("owner", "alice")
            .unwrap();
        let bob = Arc::new(Memory {
            owner: Some("capability-bob".into()),
            ..Memory::default()
        });
        *bob.settings.lock().unwrap() = Some(r#"{"resolverfixture":{"route":"bob"}}"#.into());
        configure_plugin_settings(bob.clone()).unwrap();
        assert!(crate::host::session_state().storage_get("owner").is_none());
        assert_eq!(
            owner_settings
                .with(|| crate::host::session_state().storage_get("owner"))
                .as_deref(),
            Some("alice")
        );
        crate::set_installed_packages(Vec::new());
        assert!(capability.is_current());
        assert_eq!(
            capability
                .resolve(
                    "opaque",
                    std::time::Duration::from_secs(1),
                    Arc::new(|| false)
                )
                .unwrap()
                .url
                .as_deref(),
            Some("https://example.test/alice")
        );
        let before_failed_preparation = settings_generation();
        let corrupt = Arc::new(Memory::default());
        *corrupt.settings.lock().unwrap() = Some("broken settings".into());
        assert!(prepare_plugin_settings(corrupt).is_err());
        assert_eq!(settings_generation(), before_failed_preparation);
        assert_eq!(
            settings_snapshot("resolverfixture", &[])
                .unwrap()
                .values
                .len(),
            0
        );
        configure_plugin_settings(alice.clone()).unwrap();
        assert!(
            capability.is_current(),
            "returning to the same owner with identical settings preserves transfers"
        );
        let stale_prepared = prepare_plugin_settings(alice.clone()).unwrap();
        configure_plugin_settings(bob).unwrap();
        // A retained owner can complete a credential write while its prepared
        // activation waits. Publishing the old snapshot must keep that write.
        owner_settings.with(|| {
            set_setting(
                "resolverfixture",
                "route",
                Value::String("alice-edited".into()),
            )
            .unwrap();
        });
        activate_plugin_settings(stale_prepared);
        let route: SettingField = serde_json::from_value(serde_json::json!({
            "key":"route","label":"Route","type":"string","default":""
        }))
        .unwrap();
        assert_eq!(
            settings_snapshot("resolverfixture", &[route])
                .unwrap()
                .values["route"],
            Value::String("alice-edited".into()),
            "activation must not overwrite a retained owner's newer credentials"
        );
        assert!(!capability.is_current());
        assert!(
            capability
                .resolve(
                    "opaque",
                    std::time::Duration::from_secs(1),
                    Arc::new(|| false)
                )
                .is_err()
        );
        crate::set_installed_packages(packages);
        let revoked = crate::capture_resolver_capability(
            "nova-provider://resolverfixture",
            settings_generation(),
        )
        .unwrap();
        revoke_provider("capability-alice", "nova-provider://resolverfixture");
        assert!(!revoked.is_current());
        crate::set_installed_packages(previous_packages);
        *service().lock().unwrap() = Service::default();
    }
    #[test]
    fn sort_order_requires_complete_unique_known_criteria_and_bounded_actions() {
        let field: SettingField = serde_json::from_value(serde_json::json!({
            "key":"sort.order","label":"Sort order","type":"sort-order",
            "default":[{"key":"size","direction":"desc"},{"key":"resolution","direction":"asc"}],
            "options":[{"value":"size","label":"Size"},{"value":"resolution","label":"Resolution"}]
        }))
        .unwrap();
        field.validate(&field.default).unwrap();
        for invalid in [
            serde_json::json!([]),
            serde_json::json!([{"key":"size","direction":"desc"}]),
            serde_json::json!([{"key":"size","direction":"desc"},{"key":"size","direction":"asc"}]),
            serde_json::json!([{"key":"size","direction":"desc"},{"key":"future","direction":"asc"}]),
            serde_json::json!([{"key":"size","direction":"descending"},{"key":"resolution","direction":"asc"}]),
            serde_json::json!([{"key":"size","direction":"desc","extra":true},{"key":"resolution","direction":"asc"}]),
        ] {
            assert!(field.validate(&invalid).is_err(), "{invalid}");
        }
        let moved = field
            .apply_sort_action(
                &field.default,
                &serde_json::json!({"sortAction":"down","criterion":"size"}),
            )
            .unwrap();
        assert_eq!(moved[0]["key"], "resolution");
        let toggled = field
            .apply_sort_action(
                &moved,
                &serde_json::json!({"sortAction":"toggle","criterion":"size"}),
            )
            .unwrap();
        assert_eq!(toggled[1]["direction"], "asc");
        assert_eq!(
            field
                .apply_sort_action(
                    &toggled,
                    &serde_json::json!({"sortAction":"down","criterion":"size"})
                )
                .unwrap(),
            toggled
        );
        for action in [
            serde_json::json!({"sortAction":"up","criterion":"unknown"}),
            serde_json::json!({"sortAction":"remove","criterion":"size"}),
            serde_json::json!({"sortAction":"toggle","criterion":"size","extra":0}),
        ] {
            assert!(field.apply_sort_action(&field.default, &action).is_err());
        }
        let mut oversized = field.clone();
        oversized.options = (0..17)
            .map(|index| SettingOption {
                value: Value::String(format!("metric{index}")),
                label: String::new(),
            })
            .collect();
        assert!(oversized.validate(&Value::Array((0..17).map(|index|serde_json::json!({"key":format!("metric{index}"),"direction":"desc"})).collect())).is_err());
    }
    #[test]
    fn ordered_choices_validate_actions_and_sort_schema_growth_preserves_order() {
        let field: SettingField = serde_json::from_value(serde_json::json!({
            "key":"language.prefs","label":"Preferred languages","type":"ordered-choices","default":[],
            "options":[{"value":"en","label":"English"},{"value":"ja","label":"Japanese"}]
        })).unwrap();
        field.validate(&field.default).unwrap();
        for invalid in [
            serde_json::json!(["en", "en"]),
            serde_json::json!(["xx"]),
            serde_json::json!([1]),
            serde_json::json!({}),
        ] {
            assert!(field.validate(&invalid).is_err());
        }
        let selected = serde_json::json!(["en", "ja"]);
        let moved = field
            .apply_priority_action(
                &selected,
                &serde_json::json!({"priorityAction":"up","choice":"ja"}),
            )
            .unwrap();
        assert_eq!(moved, serde_json::json!(["ja", "en"]));
        let removed = field
            .apply_priority_action(
                &moved,
                &serde_json::json!({"priorityAction":"remove","choice":"ja"}),
            )
            .unwrap();
        assert_eq!(removed, serde_json::json!(["en"]));
        assert_eq!(
            field
                .apply_priority_action(
                    &removed,
                    &serde_json::json!({"priorityAction":"add","choice":"ja"})
                )
                .unwrap(),
            selected
        );
        assert!(
            field
                .apply_priority_action(
                    &selected,
                    &serde_json::json!({"priorityAction":"add","choice":"xx"})
                )
                .is_err()
        );
        let sort: SettingField = serde_json::from_value(serde_json::json!({
            "key":"sort.order","label":"Sort order","type":"sort-order",
            "default":[{"key":"cached","direction":"desc"},{"key":"resolution","direction":"desc"},{"key":"size","direction":"desc"},{"key":"pack","direction":"desc"},{"key":"seeders","direction":"desc"},{"key":"language","direction":"desc"}],
            "options":[{"value":"cached","label":"Cached"},{"value":"resolution","label":"Resolution"},{"value":"size","label":"Size"},{"value":"pack","label":"Pack"},{"value":"seeders","label":"Seeders"},{"value":"language","label":"Language"}]
        })).unwrap();
        let legacy = serde_json::json!([{"key":"seeders","direction":"asc"},{"key":"size","direction":"asc"},{"key":"cached","direction":"desc"},{"key":"pack","direction":"desc"},{"key":"resolution","direction":"asc"}]);
        let migrated = sort.normalized_value(&legacy).unwrap();
        assert_eq!(
            &migrated.as_array().unwrap()[..5],
            legacy.as_array().unwrap()
        );
        assert_eq!(
            migrated[5],
            serde_json::json!({"key":"language","direction":"desc"})
        );
        let mut expanded = sort.clone();
        expanded.options.push(SettingOption {
            value: Value::String("codec".into()),
            label: "Preferred video codec".into(),
        });
        expanded
            .default
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"key":"codec","direction":"desc"}));
        let from_five = expanded.normalized_value(&legacy).unwrap();
        assert_eq!(
            &from_five.as_array().unwrap()[..5],
            legacy.as_array().unwrap()
        );
        assert_eq!(
            from_five[6],
            serde_json::json!({"key":"codec","direction":"desc"})
        );
        let from_six = expanded.normalized_value(&migrated).unwrap();
        assert_eq!(
            &from_six.as_array().unwrap()[..6],
            migrated.as_array().unwrap()
        );
        assert_eq!(
            from_six[6],
            serde_json::json!({"key":"codec","direction":"desc"})
        );
        assert!(
            sort.normalized_value(&serde_json::json!([{"key":"future","direction":"desc"}]))
                .is_none()
        );
    }
}

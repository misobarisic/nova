//! Account profile identities, local selection and captured asynchronous ownership.
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex, RwLock};

pub(super) const DEFAULT_ID: &str = "default";
pub(super) const DOMAIN: &str = "profiles";
pub(super) const DELETIONS_DOMAIN: &str = "profile_deletions";
const DEVICE_KEY: &str = "profiles:device:v1";
fn default_true() -> bool {
    true
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct DevicePreferences {
    #[serde(default = "default_id")]
    pub active_profile: String,
    #[serde(default = "default_true")]
    pub open_last_profile_on_startup: bool,
}
fn default_id() -> String {
    DEFAULT_ID.into()
}
impl Default for DevicePreferences {
    fn default() -> Self {
        Self {
            active_profile: default_id(),
            open_last_profile_on_startup: true,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Profile {
    pub id: String,
    pub name: String,
}
#[derive(Clone)]
pub(super) struct Context {
    pub id: String,
    pub epoch: u64,
    pub session: Option<Arc<super::profile_session::ProfileSession>>,
}
impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("id", &self.id)
            .field("epoch", &self.epoch)
            .finish()
    }
}
impl PartialEq for Context {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.epoch == other.epoch
    }
}
impl Eq for Context {}
impl std::hash::Hash for Context {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
        self.epoch.hash(state);
    }
}
struct Runtime {
    enabled: bool,
    context: Context,
    device: DevicePreferences,
    retired: HashSet<String>,
    cleaned: HashSet<String>,
}
static RUNTIME: LazyLock<RwLock<Runtime>> = LazyLock::new(|| {
    RwLock::new(Runtime {
        enabled: false,
        context: Context {
            id: default_id(),
            epoch: 1,
            session: None,
        },
        device: DevicePreferences::default(),
        retired: HashSet::new(),
        cleaned: HashSet::new(),
    })
});
// Deletion and scoped writes share this lock: an already running writer either
// finishes before cleanup, or observes retirement and cannot recreate data.
pub(super) static WRITE_SERIAL: Mutex<()> = Mutex::new(());
thread_local! { static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) }; }
pub(super) fn capture() -> Context {
    CONTEXT
        .with(|c| c.borrow().clone())
        .unwrap_or_else(|| RUNTIME.read().unwrap().context.clone())
}
/// The selected UI profile, independent of an outgoing callback's captured
/// context. Activation uses this to enter the replacement profile explicitly.
pub(super) fn active_context() -> Context {
    RUNTIME.read().unwrap().context.clone()
}
static NEXT_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(2);
pub(super) fn activation_context(id: &str) -> Context {
    Context {
        id: id.into(),
        epoch: NEXT_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        session: None,
    }
}
pub(super) fn attach_session(session: &Arc<super::profile_session::ProfileSession>) {
    let mut runtime = RUNTIME.write().unwrap();
    if runtime.enabled && runtime.context == session.context() {
        runtime.context.session = Some(session.clone());
    }
}
pub(super) fn commit_session(
    prepared: PreparedActivation,
    session: &Arc<super::profile_session::ProfileSession>,
) {
    assert_eq!(prepared.device.active_profile, session.id);
    let mut runtime = RUNTIME.write().unwrap();
    runtime.context = session.context();
    runtime.device = prepared.device;
}

pub(super) fn current_id() -> String {
    capture().id
}
pub(super) fn context_for(id: &str) -> Context {
    Context {
        id: id.into(),
        epoch: 0,
        session: None,
    }
}
pub(super) fn with_context<R>(context: Context, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Context>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CONTEXT.with(|c| *c.borrow_mut() = self.0.take());
        }
    }
    let old = CONTEXT.with(|c| c.replace(Some(context)));
    let _restore = Restore(old);
    f()
}
pub(super) fn is_current(context: &Context) -> bool {
    let state = RUNTIME.read().unwrap();
    !state.enabled || state.context == *context
}
pub(super) fn is_initialized() -> bool {
    enabled()
}
pub(super) fn enabled() -> bool {
    RUNTIME.read().unwrap().enabled
}
pub(super) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
pub(super) fn storage_key_for(id: &str, key: &str) -> String {
    format!("profile:{id}:{key}")
}
pub(super) fn storage_key(key: &str) -> String {
    ProfileStore::capture().key(key)
}
pub(super) fn domain_for(id: &str, base: &str) -> String {
    format!("profile:{id}:{base}")
}
pub(super) fn parse_domain(domain: &str) -> Option<(&str, &str)> {
    let (id, base) = domain.strip_prefix("profile:")?.split_once(':')?;
    (valid_id(id) && !base.is_empty()).then_some((id, base))
}
pub(super) fn scoped_key(key: &str) -> bool {
    matches!(
        key,
        "settings"
            | "library"
            | "episode_progress"
            | "continue_hidden"
            | "viewing_activity:v1"
            | "watch_history_hidden:v1"
            | "new_episodes_hidden:v1"
            | "playback_preferences:v1"
            | "stream_family_preferences:v1"
            | "scene_bookmarks:v1"
            | "secondary_subtitles:v1"
            | "secondary_subtitle_positions:v1"
            | "watch_queue:v1"
            | "offline_policies:v1"
            | "watch_queue:v2"
            | super::profile_policy::KEY
            | "release_subscriptions:v1"
            | "release_delivery:v1"
            | "calendar_export:v1"
            | "addons"
            | "addon_packages:v1"
            | "discover:search_history:v2"
            | "discover:search_history"
            | "home:showcase:v1"
            | "home:catalog-rows:v1"
            | "plugin_settings"
    ) || key.starts_with("viewing_journal:v1:")
        || key.starts_with("tracking:")
        || key.starts_with("similar_titles:")
        || key.starts_with("stremio:")
        || key.starts_with("list_services:")
}
pub(super) fn writable(id: &str) -> bool {
    !RUNTIME.read().unwrap().retired.contains(id)
}
pub(super) fn list() -> Result<Vec<Profile>, String> {
    let owner = nova_sync::local_store().map_err(|e| e.to_string())?;
    let store = owner.lock().unwrap();
    let deleted: HashSet<String> = store
        .records(DELETIONS_DOMAIN)
        .into_iter()
        .map(|(id, _)| id)
        .filter(|id| valid_id(id))
        .collect();
    RUNTIME
        .write()
        .unwrap()
        .retired
        .extend(deleted.iter().cloned());
    let mut profiles = Vec::new();
    for (id, raw) in store.records(DOMAIN) {
        if !valid_id(&id)
            || store
                .record(DELETIONS_DOMAIN, &id)
                .is_some_and(|r| r.value.is_some())
        {
            continue;
        }
        #[derive(Deserialize)]
        struct Name {
            name: String,
        }
        let value: Name = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        profiles.push(Profile {
            id,
            name: value.name,
        });
    }
    profiles.sort_by(|a, b| a.id.cmp(&b.id));
    drop(store);
    cleanup_deleted_data(deleted)?;
    #[cfg(target_os = "android")]
    super::release_reminders::update_android_schedule(&profiles);
    Ok(profiles)
}
fn cleanup_deleted_data(ids: HashSet<String>) -> Result<(), String> {
    for id in ids {
        super::release_reminders::cancel_profile_notifications(&id);
        nova_providers::plugin_settings::revoke_owner(
            &ProfileStore::for_profile(&id).domain("plugin_settings"),
        );
        // A stale peer may deliver live children after the deletion marker.
        // Retombstone these under the record owner's lock before considering
        // local cleanup complete; the membership marker alone hides values.
        let owner = nova_sync::local_store().map_err(|error| error.to_string())?;
        let device = nova_sync::local_device().map_err(|error| error.to_string())?;
        let mut store = owner.lock().unwrap();
        // Tracking recovery may write durable storage while holding this
        // owner. Keep the same lock order for deletion and cleanup.
        let _serial = WRITE_SERIAL.lock().unwrap();
        let prefix = format!("profile:{id}:");
        let mut changed = false;
        for domain in store.domains() {
            if domain.starts_with(&prefix) {
                for (key, _) in store.records(&domain) {
                    changed |= store.set(&domain, &key, None, 0, device);
                }
            }
        }
        if changed {
            store.save().map_err(|error| error.to_string())?;
        }
        drop(store);
        if changed {
            nova_sync::request_local_sync();
        }
        if RUNTIME.read().unwrap().cleaned.contains(&id) {
            continue;
        }
        let mut edits = Vec::new();
        for prefix in [
            format!("profile:{id}:"),
            format!("sync:baseline:profile:{id}:"),
        ] {
            for (key, _) in nova_storage::try_scan_prefix(&prefix).map_err(|e| e.to_string())? {
                edits.push((key, None));
            }
        }
        nova_storage::try_write_batch(&edits).map_err(|e| e.to_string())?;
        match std::fs::remove_dir_all(cache_dir(&id)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        RUNTIME.write().unwrap().cleaned.insert(id);
    }
    Ok(())
}
fn validated_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err("Enter a profile name of up to 64 characters.".into());
    }
    Ok(name.into())
}
fn write_profile(id: &str, name: &str) -> Result<(), String> {
    let owner = nova_sync::local_store().map_err(|e| e.to_string())?;
    let device = nova_sync::local_device().map_err(|e| e.to_string())?;
    let mut store = owner.lock().unwrap();
    if store
        .record(DELETIONS_DOMAIN, id)
        .is_some_and(|r| r.value.is_some())
    {
        return Err("This profile was deleted.".into());
    }
    store.set(
        DOMAIN,
        id,
        Some(serde_json::json!({"name":name}).to_string()),
        0,
        device,
    );
    store.save().map_err(|e| e.to_string())?;
    drop(store);
    nova_sync::request_local_sync();
    Ok(())
}
fn seed_default_in_store(store: &mut nova_sync::Store, device: u64) -> bool {
    if store.record(DOMAIN, DEFAULT_ID).is_some()
        || store
            .record(DELETIONS_DOMAIN, DEFAULT_ID)
            .is_some_and(|record| record.value.is_some())
    {
        return false;
    }
    // Default is migration scaffolding, never an explicit rename. A device
    // joining later must not replace another device's user-chosen name.
    store.apply(
        DOMAIN,
        DEFAULT_ID,
        nova_sync::Record {
            value: Some(serde_json::json!({"name":"Default"}).to_string()),
            version: nova_sync::Version::new(0, 0, device, false),
        },
    )
}
fn seed_default_profile() -> Result<bool, String> {
    let owner = nova_sync::local_store().map_err(|error| error.to_string())?;
    let device = nova_sync::local_device().map_err(|error| error.to_string())?;
    let mut store = owner.lock().unwrap();
    let changed = seed_default_in_store(&mut store, device);
    if changed {
        store.save().map_err(|error| error.to_string())?;
    }
    let available = store
        .record(DOMAIN, DEFAULT_ID)
        .is_some_and(|record| record.value.is_some())
        && !store
            .record(DELETIONS_DOMAIN, DEFAULT_ID)
            .is_some_and(|record| record.value.is_some());
    drop(store);
    if changed {
        nova_sync::request_local_sync();
    }
    Ok(available)
}
pub(super) fn initialize() -> Result<DevicePreferences, String> {
    let device: DevicePreferences =
        match nova_storage::try_get_str(DEVICE_KEY).map_err(|e| e.to_string())? {
            Some(raw) => serde_json::from_str(&raw)
                .map_err(|e| format!("Unreadable profile selection: {e}"))?,
            None => DevicePreferences::default(),
        };
    if list()?.is_empty() && (!writable(DEFAULT_ID) || !seed_default_profile()?) {
        // Concurrent offline deletes can remove every prior identity.
        // A replacement must have a new ID; deletion is permanent.
        create("Default")?;
    }
    let profiles = list()?;
    let selected = profiles
        .iter()
        .find(|p| p.id == device.active_profile)
        .unwrap_or(&profiles[0])
        .id
        .clone();
    let device = DevicePreferences {
        active_profile: selected.clone(),
        ..device
    };
    nova_storage::try_set_str(
        DEVICE_KEY,
        &serde_json::to_string(&device).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut state = RUNTIME.write().unwrap();
    state.enabled = true;
    state.context.id = selected;
    state.context.session = None;
    state.device = device.clone();
    Ok(device)
}
pub(super) fn device_preferences() -> DevicePreferences {
    RUNTIME.read().unwrap().device.clone()
}
pub(super) struct PreparedActivation {
    device: DevicePreferences,
}

/// Finish the fallible selection write while the outgoing services still run.
/// Publication happens later on the UI thread, after their shutdown is queued.
pub(super) fn prepare_activation(id: &str) -> Result<PreparedActivation, String> {
    if !list()?.iter().any(|p| p.id == id) {
        return Err("This profile is unavailable.".into());
    }
    let mut device = RUNTIME.read().unwrap().device.clone();
    device.active_profile = id.into();
    nova_storage::try_set_str(
        DEVICE_KEY,
        &serde_json::to_string(&device).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(PreparedActivation { device })
}

pub(super) fn set_open_last_profile_on_startup(enabled: bool) -> Result<(), String> {
    let mut state = RUNTIME.write().unwrap();
    let mut device = state.device.clone();
    device.open_last_profile_on_startup = enabled;
    nova_storage::try_set_str(
        DEVICE_KEY,
        &serde_json::to_string(&device).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    state.device = device;
    Ok(())
}
pub(super) fn create(name: &str) -> Result<String, String> {
    let name = validated_name(name)?;
    let device = nova_sync::local_device().map_err(|e| e.to_string())?;
    let sequence =
        nova_storage::try_allocate_local_sequence("profiles", 1).map_err(|e| e.to_string())?;
    let id = format!("p{device:x}_{sequence:x}");
    write_profile(&id, &name)?;
    Ok(id)
}
pub(super) fn rename(id: &str, name: &str) -> Result<(), String> {
    if !list()?.iter().any(|p| p.id == id) {
        return Err("This profile is unavailable.".into());
    }
    write_profile(id, &validated_name(name)?)
}
pub(super) fn delete(id: &str) -> Result<(), String> {
    let profiles = list()?;
    if profiles.len() <= 1 {
        return Err("Keep at least one profile.".into());
    }
    if !profiles.iter().any(|p| p.id == id) {
        return Err("This profile is unavailable.".into());
    }
    let owner = nova_sync::local_store().map_err(|e| e.to_string())?;
    let device = nova_sync::local_device().map_err(|e| e.to_string())?;
    let mut store = owner.lock().unwrap();
    let _serial = WRITE_SERIAL.lock().unwrap();
    RUNTIME.write().unwrap().retired.insert(id.into());
    let prefix = format!("profile:{id}:");
    for domain in store.domains() {
        if domain.starts_with(&prefix) {
            for (key, _) in store.records(&domain) {
                store.set(&domain, &key, None, 0, device);
            }
        }
    }
    store.set(DELETIONS_DOMAIN, id, Some("1".into()), 0, device);
    store.set(DOMAIN, id, None, 0, device);
    for prefix in [prefix, format!("sync:baseline:profile:{id}:")] {
        for (key, _) in nova_storage::try_scan_prefix(&prefix).map_err(|e| e.to_string())? {
            store.queue_extra(&key, None);
        }
    }
    store.save().map_err(|e| e.to_string())?;
    drop(store);
    drop(_serial);
    nova_providers::plugin_settings::revoke_owner(
        &ProfileStore::for_profile(id).domain("plugin_settings"),
    );
    let _ = std::fs::remove_dir_all(cache_dir(id));
    nova_sync::request_local_sync();
    Ok(())
}
pub(super) fn cache_dir(id: &str) -> std::path::PathBuf {
    nova_config::app_cache_dir().join("profiles").join(id)
}
pub(super) fn legacy_snapshot_migration_entries()
-> Result<Vec<(String, Option<String>)>, nova_storage::Error> {
    let mut entries = Vec::new();
    for (key, raw) in nova_storage::try_scan_prefix("")? {
        // These checkpoints were introduced after profiles. An unowned row
        // cannot prove which person's local watch action authorized an upload.
        if scoped_key(&key) && !key.starts_with("list_services:") {
            let target = storage_key_for(DEFAULT_ID, &key);
            if nova_storage::try_get_str(&target)?.is_none() {
                entries.push((target, Some(raw)));
            }
            entries.push((key, None));
        }
    }
    Ok(entries)
}
pub(super) fn invoke_from_event_loop(
    f: impl FnOnce() + Send + 'static,
) -> Result<(), slint::EventLoopError> {
    let context = capture();
    slint::invoke_from_event_loop(move || {
        if is_current(&context) {
            with_context(context, f);
        }
    })
}
pub(super) fn invoke_account(
    f: impl FnOnce() + Send + 'static,
) -> Result<(), slint::EventLoopError> {
    slint::invoke_from_event_loop(f)
}

pub(super) mod thread {
    #[cfg(feature = "desktop")]
    pub use std::thread::available_parallelism;
    pub use std::thread::{JoinHandle, sleep};
    pub fn spawn<F, T>(f: F) -> JoinHandle<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let ctx = super::capture();
        std::thread::spawn(move || super::with_context(ctx, f))
    }
    pub struct Builder(std::thread::Builder);
    impl Builder {
        pub fn new() -> Self {
            Self(std::thread::Builder::new())
        }
        pub fn name(self, name: String) -> Self {
            Self(self.0.name(name))
        }
        pub fn spawn<F, T>(self, f: F) -> std::io::Result<JoinHandle<T>>
        where
            F: FnOnce() -> T + Send + 'static,
            T: Send + 'static,
        {
            let ctx = super::capture();
            self.0.spawn(move || super::with_context(ctx, f))
        }
    }
}

/// Tracking's leaf store uses the same captured namespace as app-side events.
/// The default leaf KvStorage intentionally remains raw for independent users.
#[derive(Clone)]
pub(super) struct TrackingStorage {
    owner: ProfileStore,
}
impl TrackingStorage {
    pub(super) fn capture() -> Self {
        Self::for_store(ProfileStore::capture())
    }
    pub(super) fn for_store(owner: ProfileStore) -> Self {
        Self { owner }
    }
    pub(super) fn profile_store(&self) -> &ProfileStore {
        &self.owner
    }
}
impl nova_tracking::StateStorage for TrackingStorage {
    fn read(&self, key: &str) -> Result<Option<String>, nova_storage::Error> {
        self.owner.read(key)
    }
    fn write(&self, entries: &[(String, Option<String>)]) -> Result<(), nova_storage::Error> {
        self.owner.write_batch(entries)
    }
}

pub(super) mod storage;
pub(super) mod store;
pub(super) use store::ProfileStore;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn untouched_default_cannot_replace_an_explicit_peer_rename() {
        let mut named = nova_sync::Store::default();
        named.set(DOMAIN, DEFAULT_ID, Some(r#"{"name":"Alice"}"#.into()), 0, 1);
        let explicit = named.record(DOMAIN, DEFAULT_ID).unwrap().clone();
        let mut joining = nova_sync::Store::default();
        assert!(seed_default_in_store(&mut joining, 2));
        let neutral = joining.record(DOMAIN, DEFAULT_ID).unwrap().clone();
        assert!(!named.apply(DOMAIN, DEFAULT_ID, neutral));
        assert!(joining.apply(DOMAIN, DEFAULT_ID, explicit));
        assert_eq!(joining.records(DOMAIN), named.records(DOMAIN));
        assert!(!seed_default_in_store(&mut joining, 2));
    }
    #[test]
    fn startup_selection_is_opt_out_and_preserves_an_explicit_choice() {
        let missing: DevicePreferences = serde_json::from_str("{}").unwrap();
        assert!(missing.open_last_profile_on_startup);
        let disabled: DevicePreferences =
            serde_json::from_str(r#"{"open_last_profile_on_startup":false}"#).unwrap();
        assert!(!disabled.open_last_profile_on_startup);
    }
    #[test]
    fn captured_context_survives_workers_and_restores_after_unwinding() {
        let before = capture();
        let selected = context_for("alice");
        with_context(selected.clone(), || {
            assert_eq!(thread::spawn(capture).join().unwrap(), selected);
            let _ = std::panic::catch_unwind(|| {
                with_context(context_for("bob"), || panic!("test unwind"))
            });
            assert_eq!(capture(), selected);
        });
        assert_eq!(capture(), before);
    }
    #[test]
    fn namespaces_keep_device_keys_and_profile_identity_separate() {
        assert_eq!(
            parse_domain("profile:alice:tracking:state:v1"),
            Some(("alice", "tracking:state:v1"))
        );
        assert_eq!(parse_domain("profile:../alice:settings"), None);
        assert!(scoped_key("tracking:credentials:mal:v1"));
        assert!(scoped_key("stremio:state:v1:account"));
        assert!(scoped_key("list_services:watched:v1:title"));
        assert!(scoped_key("list_services:boundary:v1:account"));
        assert!(scoped_key("list_services:simkl_cache:v1:account"));
        assert!(scoped_key("plugin_settings"));
        assert!(scoped_key("watch_queue:v1"));
        assert!(scoped_key("watch_queue:v2"));
        assert!(scoped_key(crate::app::profile_policy::KEY));
        assert!(scoped_key("scene_bookmarks:v1"));
        assert!(scoped_key("secondary_subtitles:v1"));
        assert!(scoped_key("secondary_subtitle_positions:v1"));
        assert!(scoped_key("viewing_journal:v1:session-1"));
        assert!(scoped_key("similar_titles:v1:series\u{1}tt1910272"));
        for key in [
            "downloads:v1",
            "download_settings",
            "torrent_settings",
            "sync:identity",
            "desktop:window:v1",
            DEVICE_KEY,
        ] {
            assert!(!scoped_key(key), "{key}");
        }
    }
}

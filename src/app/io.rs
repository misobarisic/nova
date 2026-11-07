//! Small shared helpers: KV JSON IO, atomic file writes, hashing,
//! and human-readable formatting.
use super::*;
static UNREADABLE_KEYS: Mutex<Option<HashSet<String>>> = Mutex::new(None);
// Invalid incoming records must not let local derived writes overwrite the
// retained snapshot. Keep this separate from corrupt native bytes: a valid
// remote correction may repair this guard only after its commit succeeds.
static INVALID_PROJECTIONS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);
static NEXT_PROJECTION_GUARD: AtomicU64 = AtomicU64::new(1);
static WRITE_FAILURES: AtomicU64 = AtomicU64::new(0);
pub(crate) fn write_failures() -> u64 {
    WRITE_FAILURES.load(Ordering::Relaxed)
}
pub(super) fn record_write_failure(error: storage::Error) {
    WRITE_FAILURES.fetch_add(1, Ordering::Relaxed);
    storage::report(error);
}
pub(crate) fn block_unreadable(key: &str) {
    UNREADABLE_KEYS
        .lock()
        .unwrap()
        .get_or_insert_with(HashSet::new)
        .insert(profiles::storage_key(key));
}
pub(crate) fn writable_key(key: &str) -> bool {
    projection_writable_key(key)
        && !INVALID_PROJECTIONS
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|keys| keys.contains_key(&profiles::storage_key(key)))
}
pub(crate) fn projection_writable_key(key: &str) -> bool {
    !UNREADABLE_KEYS
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|keys| keys.contains(&profiles::storage_key(key)))
}
pub(crate) fn block_projection(key: &str) {
    INVALID_PROJECTIONS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(
            profiles::storage_key(key),
            NEXT_PROJECTION_GUARD.fetch_add(1, Ordering::Relaxed),
        );
}
pub(crate) fn projection_guard(key: &str) -> Option<u64> {
    INVALID_PROJECTIONS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|keys| keys.get(&profiles::storage_key(key)).copied())
}
/// Called under the Store owner lock only after its captured basis was fully
/// acknowledged. A later malformed attempt owns a different guard ticket.
pub(crate) fn projection_acknowledged(key: &str, captured: Option<u64>) {
    let Some(captured) = captured else {
        return;
    };
    if let Some(keys) = INVALID_PROJECTIONS.lock().unwrap().as_mut() {
        let key = profiles::storage_key(key);
        if keys.get(&key) == Some(&captured) {
            keys.remove(&key);
        }
    }
}

/// Only local metadata is eligible; user state and sync wire values retain
/// their existing JSON representation and publication paths.
pub(crate) fn metadata_cache_key(key: &str) -> bool {
    key.starts_with("episodes:")
        || key.starts_with("meta_header:")
        || key.starts_with("similar_titles:")
        || key.starts_with("manifest:")
        || matches!(
            key,
            "provider_metadata_cache:v1"
                | "catalog_grouping_evidence:v1"
                | "plugin_cache:v1"
                | "home:showcase:v1"
                | "tracking:catalog_cache:v1"
        )
}

pub(crate) fn read_json_result<T: serde::de::DeserializeOwned>(
    key: &str,
) -> Result<Option<T>, storage::Error> {
    let raw = storage::try_get_str(key)?;
    raw.map(|s| {
        serde_json::from_str(&s).map_err(|e| storage::Error::new(storage::ErrorKind::Schema, e))
    })
    .transpose()
}

/// Deserialize a JSON string from the KV store.
pub(crate) fn read_json<T: serde::de::DeserializeOwned>(key: &str) -> Option<T> {
    match read_json_result(key) {
        Ok(value) => value,
        Err(e) => {
            block_unreadable(key);
            storage::report(e);
            None
        }
    }
}
/// Serialize a value to JSON and write it to the KV store.
pub(crate) fn write_json(key: &str, value: &(impl Serialize + ?Sized)) {
    if !writable_key(key) {
        storage::report(storage::Error::new(
            storage::ErrorKind::Schema,
            "unreadable app data retained; writes blocked until recovery",
        ));
        WRITE_FAILURES.fetch_add(1, Ordering::Relaxed);
        return;
    }
    match serde_json::to_string(value) {
        Ok(s) => {
            if let Err(e) = persist_sync_snapshot(key, &s, false) {
                WRITE_FAILURES.fetch_add(1, Ordering::Relaxed);
                storage::report(storage::Error::new(
                    storage::ErrorKind::Transaction,
                    format!("persistence failed: {e:#}"),
                ));
            }
        }
        Err(e) => {
            WRITE_FAILURES.fetch_add(1, Ordering::Relaxed);
            storage::report(storage::Error::new(storage::ErrorKind::Serialization, e));
        }
    }
}
/// Human-readable disk usage for the Settings page, e.g.
/// `"128 MiB · 1,203 files"`. Pure (cross-platform for tests).
pub(crate) fn format_disk_usage(bytes: u64, files: usize) -> String {
    format!(
        "{} · {}",
        nova_download::format_bytes(bytes),
        text::files_label(files)
    )
}
/// Human-readable transfer rate for the player status line, e.g.
/// `"1.2 MiB"` for 1_258_291 bytes/second (callers append `/s`).
pub(crate) fn format_rate(bytes_per_sec: u64) -> String {
    nova_download::format_bytes(bytes_per_sec)
}
/// Write `contents` to `path` via a unique temp file + rename, flushing the
/// file and its directory, so a crash mid-write never leaves a truncated file
/// behind and concurrent writers never share a staging file.
#[allow(dead_code)]
#[cfg(feature = "desktop")]
pub(crate) fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    nova_storage::fs::atomic_write(path, contents.as_bytes())
}
/// FNV-1a 64-bit; used only to derive stable, filesystem-safe cache file
/// names (manifests, poster originals) from their URLs.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Device-local cache of confirmed provider mappings; never synced.
pub(super) struct ProviderMetadataCache;
impl nova_providers::MetadataCache for ProviderMetadataCache {
    fn load(&self) -> Result<Option<String>, nova_providers::ProviderHostError> {
        storage::try_get_str("provider_metadata_cache:v1")
            .map_err(|error| nova_providers::ProviderHostError(error.to_string()))
    }
    fn save(&self, value: &str) -> Result<(), nova_providers::ProviderHostError> {
        storage::try_set_cached_str("provider_metadata_cache:v1", value)
            .map_err(|error| nova_providers::ProviderHostError(error.to_string()))
    }
}

/// Catalog relationships are derived device-local evidence, not user settings.
pub(super) struct CatalogGroupingCache;
impl nova_providers::MetadataCache for CatalogGroupingCache {
    fn load(&self) -> Result<Option<String>, nova_providers::ProviderHostError> {
        storage::try_get_str("catalog_grouping_evidence:v1")
            .map_err(|error| nova_providers::ProviderHostError(error.to_string()))
    }
    fn save(&self, value: &str) -> Result<(), nova_providers::ProviderHostError> {
        storage::try_set_cached_str("catalog_grouping_evidence:v1", value)
            .map_err(|error| nova_providers::ProviderHostError(error.to_string()))
    }
}

#[cfg(test)]
mod cache_policy_tests {
    use super::metadata_cache_key;

    #[test]
    fn compression_is_limited_to_local_metadata() {
        for key in [
            "episodes:series\u{1}tt123",
            "meta_header:anime\u{1}kitsu:1",
            "similar_titles:v1:series\u{1}tt1910272",
            "manifest:https://addon",
            "provider_metadata_cache:v1",
            "home:showcase:v1",
            "tracking:catalog_cache:v1",
        ] {
            assert!(metadata_cache_key(key), "{key}");
        }
        for key in [
            "settings",
            "library",
            "episode_progress",
            "downloads:v1",
            "addons",
            "sync:records",
            "srec:8:settingslanguage",
            "tracking:credentials:mal:v1",
            "tracking:source:series",
        ] {
            assert!(!metadata_cache_key(key), "{key}");
        }
    }
}

#[cfg(all(test, feature = "desktop"))]
mod atomic_write_tests {
    use super::atomic_write;
    use std::{fs, path::PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("nova-atomic-write-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn a_neighbouring_tmp_file_is_not_consumed_as_the_staging_file() {
        let directory = scratch("foreign");
        let target = directory.join("episodes.json");
        // Another writer's (or a crashed one's) file at the old fixed name.
        let foreign = directory.join("episodes.tmp");
        fs::write(&foreign, "foreign").unwrap();
        atomic_write(&target, "new").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(fs::read_to_string(&foreign).unwrap(), "foreign");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn concurrent_writers_to_one_path_all_succeed_with_a_complete_result() {
        let directory = scratch("concurrent");
        let target = std::sync::Arc::new(directory.join("shared.json"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|writer| {
                let (target, barrier) = (target.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    for round in 0..40 {
                        atomic_write(&target, &format!("{writer}-{round};").repeat(2048)).unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let text = fs::read_to_string(&*target).unwrap();
        let unit = text.split(';').next().unwrap().to_owned() + ";";
        assert_eq!(text, unit.repeat(2048));
        let leftovers: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let _ = fs::remove_dir_all(directory);
    }
}

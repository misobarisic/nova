//! Small shared helpers: KV JSON IO, atomic file writes, hashing,
//! and human-readable formatting.
use super::*;
static UNREADABLE_KEYS: Mutex<Option<HashSet<String>>> = Mutex::new(None);
static WRITE_FAILURES: AtomicU64 = AtomicU64::new(0);
pub(crate) fn write_failures() -> u64 {
    WRITE_FAILURES.load(Ordering::Relaxed)
}
pub(crate) fn block_unreadable(key: &str) {
    UNREADABLE_KEYS
        .lock()
        .unwrap()
        .get_or_insert_with(HashSet::new)
        .insert(key.to_string());
}
pub(crate) fn writable_key(key: &str) -> bool {
    !UNREADABLE_KEYS
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|keys| keys.contains(key))
}

/// Only local metadata is eligible; user state and sync wire values retain
/// their existing JSON representation and publication paths.
pub(crate) fn metadata_cache_key(key: &str) -> bool {
    key.starts_with("episodes:")
        || key.starts_with("meta_header:")
        || key.starts_with("manifest:")
        || matches!(
            key,
            "provider_metadata_cache:v1" | "home:showcase:v1" | "tracking:catalog_cache:v1"
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
    if UNREADABLE_KEYS
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|keys| keys.contains(key))
    {
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
/// `"128.4 MB · 1,203 files"`. Pure (cross-platform for tests).
pub(crate) fn format_disk_usage(bytes: u64, files: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    let size = if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    };
    format!("{size} · {}", text::files_label(files))
}
/// Human-readable transfer rate for the player status line, e.g.
/// `"1.2 MB"` for 1_258_291 bytes/second.
pub(crate) fn format_rate(bytes_per_sec: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes_per_sec as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes_per_sec} B")
    }
}
/// Write `contents` to `path` via a temp file + rename, so a crash mid-write
/// never leaves a truncated file behind.
#[allow(dead_code)]
#[cfg(feature = "desktop")]
pub(crate) fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
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

#[cfg(test)]
mod cache_policy_tests {
    use super::metadata_cache_key;

    #[test]
    fn compression_is_limited_to_local_metadata() {
        for key in [
            "episodes:series\u{1}tt123",
            "meta_header:anime\u{1}kitsu:1",
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

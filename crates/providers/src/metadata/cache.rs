//! Durable confirmed mappings, separate from the short-lived JS/session store.
use super::{EnrichmentResult, now};
use crate::ProviderHostError;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock},
};

const ENTRY_LIMIT: usize = 256 * 1024;
const STORE_LIMIT: usize = 8 * 1024 * 1024;
const COUNT_LIMIT: usize = 128;

/// The application owns storage; the provider crate never opens a database.
pub trait MetadataCache: Send + Sync {
    fn load(&self) -> Result<Option<String>, ProviderHostError>;
    fn save(&self, value: &str) -> Result<(), ProviderHostError>;
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    saved_at: u64,
    value: String,
}
#[derive(Default)]
struct Cache {
    backend: Option<Arc<dyn MetadataCache>>,
    entries: BTreeMap<String, Entry>,
}
impl Cache {
    fn load(backend: Arc<dyn MetadataCache>) -> Result<Self, ProviderHostError> {
        let mut entries: BTreeMap<String, Entry> = match backend.load()? {
            Some(raw) if raw.len() <= STORE_LIMIT => serde_json::from_str(&raw)
                .map_err(|e| ProviderHostError(format!("unreadable metadata cache: {e}")))?,
            Some(_) => {
                return Err(ProviderHostError(
                    "metadata cache exceeds size limit".into(),
                ));
            }
            None => BTreeMap::new(),
        };
        entries.retain(|key, entry| {
            key.starts_with("metadata:v2:")
                && entry.value.len() <= ENTRY_LIMIT
                && serde_json::from_str::<(u64, EnrichmentResult)>(&entry.value)
                    .is_ok_and(|(_, result)| result.status == "confirmed")
        });
        while entries.len() > COUNT_LIMIT {
            Self::evict(&mut entries);
        }
        Ok(Self {
            backend: Some(backend),
            entries,
        })
    }
    fn evict(entries: &mut BTreeMap<String, Entry>) {
        if let Some(key) = entries
            .iter()
            .min_by_key(|(_, entry)| entry.saved_at)
            .map(|(key, _)| key.clone())
        {
            entries.remove(&key);
        }
    }
    fn write(&mut self, key: &str, raw: Option<&str>) -> Result<(), ProviderHostError> {
        let Some(backend) = &self.backend else {
            return Ok(());
        };
        if raw.is_some_and(|value| value.len() > ENTRY_LIMIT) {
            return Ok(());
        }
        let mut entries = self.entries.clone();
        if let Some(value) = raw {
            entries.insert(
                key.into(),
                Entry {
                    saved_at: now(),
                    value: value.into(),
                },
            );
        } else {
            entries.remove(key);
        }
        while entries.len() > COUNT_LIMIT {
            Self::evict(&mut entries);
        }
        let mut blob =
            serde_json::to_string(&entries).map_err(|e| ProviderHostError(e.to_string()))?;
        while blob.len() > STORE_LIMIT {
            Self::evict(&mut entries);
            blob = serde_json::to_string(&entries).map_err(|e| ProviderHostError(e.to_string()))?;
        }
        // Publish only after a successful durable write. Failures leave the
        // previous known-good mappings available for the next request.
        backend.save(&blob)?;
        self.entries = entries;
        Ok(())
    }
}
fn store() -> &'static Mutex<Cache> {
    static STORE: OnceLock<Mutex<Cache>> = OnceLock::new();
    STORE.get_or_init(Default::default)
}

pub fn set_metadata_cache(backend: Arc<dyn MetadataCache>) {
    match Cache::load(backend) {
        Ok(cache) => *store().lock().unwrap() = cache,
        // Do not overwrite an unreadable durable blob with an empty cache.
        Err(error) => eprintln!("nova metadata cache: {error}"),
    }
}
pub(super) fn read(key: &str) -> Option<String> {
    store()
        .lock()
        .unwrap()
        .entries
        .get(key)
        .map(|entry| entry.value.clone())
}
pub(super) fn write(key: &str, result: &EnrichmentResult, raw: &str) {
    // Missing/failed discovery never replaces a confirmed durable mapping.
    // Explicit conflicting evidence invalidates it instead of resurrecting it.
    let value = match result.status.as_str() {
        "confirmed" => Some(raw),
        "ambiguous" => None,
        _ => return,
    };
    if let Err(error) = store().lock().unwrap().write(key, value) {
        eprintln!("nova metadata cache: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Disk(Mutex<Option<String>>);
    impl MetadataCache for Disk {
        fn load(&self) -> Result<Option<String>, ProviderHostError> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn save(&self, value: &str) -> Result<(), ProviderHostError> {
            *self.0.lock().unwrap() = Some(value.into());
            Ok(())
        }
    }
    fn confirmed() -> String {
        let result = EnrichmentResult {
            supplemental_seasons: vec![],
            inventory_revision: 7,
            status: "confirmed".into(),
            details: Default::default(),
            connections: vec![],
            episode_metadata: BTreeMap::from([(
                "anikoto:ep:part2-one".into(),
                super::super::EpisodeEnrichment {
                    ids: vec!["tt5607616:2:14".into()],
                    season: Some(2),
                    episode: Some(14),
                    thumbnail: Some("https://images.example/14.jpg".into()),
                    ..Default::default()
                },
            )]),
        };
        serde_json::to_string(&(now(), result)).unwrap()
    }
    #[test]
    fn confirmed_cache_survives_restart_and_explicit_invalidation() {
        let disk = Arc::new(Disk::default());
        let mut cache = Cache::load(disk.clone()).unwrap();
        cache.write("metadata:v2:show", Some(&confirmed())).unwrap();
        drop(cache);
        let mut restarted = Cache::load(disk.clone()).unwrap();
        let (_, restored): (u64, EnrichmentResult) =
            serde_json::from_str(&restarted.entries["metadata:v2:show"].value).unwrap();
        let mapping = &restored.episode_metadata["anikoto:ep:part2-one"];
        assert_eq!(mapping.ids, ["tt5607616:2:14"]);
        assert_eq!(mapping.episode, Some(14));
        assert_eq!(
            mapping.thumbnail.as_deref(),
            Some("https://images.example/14.jpg")
        );
        restarted.write("metadata:v2:show", None).unwrap();
        assert!(Cache::load(disk).unwrap().entries.is_empty());
    }
    #[test]
    fn cache_is_bounded_and_preserves_corrupt_storage() {
        let disk = Arc::new(Disk::default());
        let mut cache = Cache::load(disk.clone()).unwrap();
        for i in 0..COUNT_LIMIT + 3 {
            cache
                .write(&format!("metadata:v2:{i}"), Some(&confirmed()))
                .unwrap();
        }
        assert_eq!(
            Cache::load(disk.clone()).unwrap().entries.len(),
            COUNT_LIMIT
        );
        cache
            .write("metadata:v2:huge", Some(&"x".repeat(ENTRY_LIMIT + 1)))
            .unwrap();
        assert!(!cache.entries.contains_key("metadata:v2:huge"));
        disk.save("corrupt").unwrap();
        assert!(Cache::load(disk.clone()).is_err());
        assert_eq!(disk.load().unwrap().as_deref(), Some("corrupt"));
    }
}

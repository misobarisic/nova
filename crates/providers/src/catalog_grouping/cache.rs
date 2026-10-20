//! Small device-local evidence index; this is separate from full enrichment
//! results and from profile settings/sync. Negative results cool down briefly.
use super::{GroupingEvidence, SourceIdentity};
use crate::MetadataCache;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock},
};

const CACHE_VERSION: u32 = 2;
const ENTRY_LIMIT: usize = 256 * 1024;
const STORE_LIMIT: usize = 512 * 1024;
const COUNT_LIMIT: usize = 128;
const POSITIVE_TTL: u64 = 7 * 24 * 3600;
const NEGATIVE_TTL: u64 = 30;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    version: u32,
    revision: u64,
    at: u64,
    source: SourceIdentity,
    evidence: Option<GroupingEvidence>,
}

#[derive(Default)]
struct Cache {
    backend: Option<Arc<dyn MetadataCache>>,
    entries: BTreeMap<String, Entry>,
}

fn store() -> &'static Mutex<Cache> {
    static STORE: OnceLock<Mutex<Cache>> = OnceLock::new();
    STORE.get_or_init(Default::default)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn key(source: &SourceIdentity, revision: u64) -> String {
    super::digest(&(CACHE_VERSION, revision, source))
}

impl Entry {
    fn valid(&self) -> bool {
        self.version == CACHE_VERSION
            && self
                .evidence
                .as_ref()
                .is_none_or(|evidence| evidence.source == self.source && evidence.verified())
            && now().saturating_sub(self.at)
                < if self.evidence.is_some() {
                    POSITIVE_TTL
                } else {
                    NEGATIVE_TTL
                }
            && serde_json::to_vec(self).is_ok_and(|raw| raw.len() <= ENTRY_LIMIT)
    }
}

fn evict(entries: &mut BTreeMap<String, Entry>) {
    if let Some(key) = entries
        .iter()
        .min_by_key(|(key, entry)| (entry.at, *key))
        .map(|(key, _)| key.clone())
    {
        entries.remove(&key);
    }
}

/// The app supplies a dedicated local-cache KV backend, never a synced key.
/// An unreadable store is retained on disk until an explicit successful reset.
pub fn set_catalog_grouping_cache(backend: Arc<dyn MetadataCache>) {
    if let Some(cache) = Cache::load(backend) {
        *store().lock().unwrap() = cache;
    }
}

impl Cache {
    fn load(backend: Arc<dyn MetadataCache>) -> Option<Self> {
        let mut entries = match backend.load().ok()? {
            Some(raw) if raw.len() <= STORE_LIMIT => {
                serde_json::from_str::<BTreeMap<String, Entry>>(&raw).ok()?
            }
            None => BTreeMap::new(),
            _ => return None,
        };
        entries
            .retain(|key, entry| entry.valid() && *key == self::key(&entry.source, entry.revision));
        while entries.len() > COUNT_LIMIT {
            evict(&mut entries);
        }
        Some(Self {
            backend: Some(backend),
            entries,
        })
    }

    fn remember(
        &mut self,
        source: &SourceIdentity,
        revision: u64,
        evidence: Option<GroupingEvidence>,
    ) {
        if evidence
            .as_ref()
            .is_some_and(|evidence| evidence.source != *source || !evidence.verified())
        {
            return;
        }
        let entry = Entry {
            version: CACHE_VERSION,
            revision,
            at: now(),
            source: source.clone(),
            evidence,
        };
        if !serde_json::to_vec(&entry).is_ok_and(|raw| raw.len() <= ENTRY_LIMIT) {
            return;
        }
        let mut entries = self.entries.clone();
        entries.retain(|_, entry| entry.valid());
        entries.insert(key(source, revision), entry);
        while entries.len() > COUNT_LIMIT {
            evict(&mut entries);
        }
        let mut raw = serde_json::to_string(&entries).unwrap_or_default();
        while raw.len() > STORE_LIMIT {
            evict(&mut entries);
            raw = serde_json::to_string(&entries).unwrap_or_default();
        }
        if self
            .backend
            .as_ref()
            .is_none_or(|backend| backend.save(&raw).is_ok())
        {
            self.entries = entries;
        }
    }
}

/// None = not cached; Some(None) = a recent unconfirmed/failed lookup.
pub fn cached_evidence(source: &SourceIdentity, revision: u64) -> Option<Option<GroupingEvidence>> {
    store()
        .lock()
        .unwrap()
        .entries
        .get(&key(source, revision))
        .filter(|entry| entry.valid())
        .map(|entry| entry.evidence.clone())
}

pub fn remember_evidence(
    source: &SourceIdentity,
    revision: u64,
    evidence: Option<GroupingEvidence>,
) {
    store().lock().unwrap().remember(source, revision, evidence);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderHostError;
    use crate::catalog_grouping::{GroupingProof, ParentIdentity, SeasonMembership};

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
    fn source() -> SourceIdentity {
        SourceIdentity {
            addon_url: "https://owner.example/secret".into(),
            media_type: "series".into(),
            id: "mal:1".into(),
        }
    }
    fn evidence(source: &SourceIdentity) -> GroupingEvidence {
        GroupingEvidence {
            source: source.clone(),
            parents: vec![ParentIdentity::ImdbSeries("tt1".into())],
            season: SeasonMembership::Season(1),
            season_partitions: vec![],
            source_inventory_fingerprint: None,
            proofs: vec![GroupingProof::ExplicitParent {
                provider_id: "opaque-provider-hash".into(),
            }],
        }
    }

    #[test]
    fn durable_evidence_keeps_owner_and_inventory_revisions_separate() {
        let disk = Arc::new(Disk::default());
        let mut cache = Cache::load(disk.clone()).unwrap();
        let first = source();
        cache.remember(&first, 7, Some(evidence(&first)));
        let mut other = first.clone();
        other.addon_url = "https://other.example/secret".into();
        cache.remember(&other, 7, None);
        cache.remember(&first, 8, None);
        let restored = Cache::load(disk).unwrap();
        assert!(restored.entries[&key(&first, 7)].evidence.is_some());
        assert!(restored.entries[&key(&other, 7)].evidence.is_none());
        assert!(restored.entries[&key(&first, 8)].evidence.is_none());
    }

    #[test]
    fn cache_is_bounded_negative_entries_expire_and_corrupt_storage_is_preserved() {
        let disk = Arc::new(Disk::default());
        let mut cache = Cache::load(disk.clone()).unwrap();
        for i in 0..COUNT_LIMIT + 3 {
            let mut source = source();
            source.id = format!("mal:{i}");
            cache.remember(&source, 7, None);
        }
        assert_eq!(cache.entries.len(), COUNT_LIMIT);
        let mut old = Entry {
            version: CACHE_VERSION,
            revision: 7,
            at: now().saturating_sub(NEGATIVE_TTL + 1),
            source: source(),
            evidence: None,
        };
        assert!(!old.valid());
        old.evidence = Some(evidence(&old.source));
        assert!(old.valid());
        disk.save("not-json").unwrap();
        assert!(Cache::load(disk.clone()).is_none());
        assert_eq!(disk.load().unwrap().as_deref(), Some("not-json"));
    }
}

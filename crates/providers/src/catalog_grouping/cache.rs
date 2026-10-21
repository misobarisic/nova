//! Small device-local evidence index; this is separate from full enrichment
//! results and from profile settings/sync. Negative results cool down briefly.
use super::{GroupingEvidence, SourceIdentity};
use crate::MetadataCache;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock},
};

// Version 5 keeps opt-in inferred results (including negatives) separate from
// confirmed resolution. Old off-policy negatives cannot suppress inference.
const CACHE_VERSION: u32 = 5;
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
    #[serde(default)]
    inferred_policy: bool,
}

#[derive(Clone, Default)]
struct Snapshot {
    backend: Option<Arc<dyn MetadataCache>>,
    entries: Arc<BTreeMap<String, Entry>>,
}

#[derive(Default)]
struct Cache {
    /// Readers take only an immutable snapshot; persistence never owns this lock.
    published: Mutex<Snapshot>,
    /// Serialize writers and backend changes without blocking snapshot readers.
    writer: Mutex<()>,
}

fn store() -> &'static Cache {
    static STORE: OnceLock<Cache> = OnceLock::new();
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

fn inference_key(source: &SourceIdentity, revision: u64) -> String {
    super::digest(&(CACHE_VERSION, "inferred", revision, source))
}

impl Entry {
    fn cache_key(&self) -> String {
        if self.inferred_policy {
            inference_key(&self.source, self.revision)
        } else {
            key(&self.source, self.revision)
        }
    }

    fn live(&self) -> bool {
        self.version == CACHE_VERSION
            && now().saturating_sub(self.at)
                < if self.evidence.is_some() {
                    POSITIVE_TTL
                } else {
                    NEGATIVE_TTL
                }
    }

    fn valid(&self) -> bool {
        self.version == CACHE_VERSION
            && self.evidence.as_ref().is_none_or(|evidence| {
                evidence.source == self.source
                    && evidence.verified()
                    && evidence.is_inferred() == self.inferred_policy
            })
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
    store().replace_backend(backend);
}

impl Snapshot {
    fn load(backend: Arc<dyn MetadataCache>) -> Option<Self> {
        let mut entries = match backend.load().ok()? {
            Some(raw) if raw.len() <= STORE_LIMIT => {
                serde_json::from_str::<BTreeMap<String, Entry>>(&raw).ok()?
            }
            None => BTreeMap::new(),
            _ => return None,
        };
        entries.retain(|key, entry| entry.valid() && *key == entry.cache_key());
        while entries.len() > COUNT_LIMIT {
            evict(&mut entries);
        }
        Some(Self {
            backend: Some(backend),
            entries: Arc::new(entries),
        })
    }
}

impl Cache {
    #[cfg(test)]
    fn load(backend: Arc<dyn MetadataCache>) -> Option<Self> {
        Some(Self {
            published: Mutex::new(Snapshot::load(backend)?),
            writer: Mutex::new(()),
        })
    }

    fn snapshot(&self) -> Snapshot {
        self.published.lock().unwrap().clone()
    }

    fn replace_backend(&self, backend: Arc<dyn MetadataCache>) {
        let _writer = self.writer.lock().unwrap();
        // Load/parse outside the publication lock. Failure retains both the
        // previous readable snapshot and the unreadable blob without a write.
        if let Some(snapshot) = Snapshot::load(backend) {
            let previous = std::mem::replace(&mut *self.published.lock().unwrap(), snapshot);
            drop(previous);
        }
    }

    fn remember(&self, source: &SourceIdentity, revision: u64, evidence: Option<GroupingEvidence>) {
        let inferred_policy = evidence.as_ref().is_some_and(GroupingEvidence::is_inferred);
        self.remember_with_policy(source, revision, evidence, inferred_policy);
    }

    fn remember_with_policy(
        &self,
        source: &SourceIdentity,
        revision: u64,
        evidence: Option<GroupingEvidence>,
        inferred_policy: bool,
    ) {
        if evidence.as_ref().is_some_and(|evidence| {
            evidence.source != *source
                || !evidence.verified()
                || evidence.is_inferred() != inferred_policy
        }) {
            return;
        }
        let entry = Entry {
            version: CACHE_VERSION,
            revision,
            at: now(),
            source: source.clone(),
            evidence,
            inferred_policy,
        };
        if !serde_json::to_vec(&entry).is_ok_and(|raw| raw.len() <= ENTRY_LIMIT) {
            return;
        }
        let _writer = self.writer.lock().unwrap();
        let previous = self.snapshot();
        let mut entries = (*previous.entries).clone();
        entries.retain(|_, entry| entry.live());
        entries.insert(entry.cache_key(), entry);
        while entries.len() > COUNT_LIMIT {
            evict(&mut entries);
        }
        let mut raw = serde_json::to_string(&entries).unwrap_or_default();
        while raw.len() > STORE_LIMIT {
            evict(&mut entries);
            raw = serde_json::to_string(&entries).unwrap_or_default();
        }
        // Keep the old immutable map visible until the durable replacement
        // succeeds. Cloning, validation, serialization and save hold no reader lock.
        if previous
            .backend
            .as_ref()
            .is_none_or(|backend| backend.save(&raw).is_ok())
        {
            let previous = std::mem::replace(
                &mut self.published.lock().unwrap().entries,
                Arc::new(entries),
            );
            drop(previous);
        }
    }

    fn get(&self, source: &SourceIdentity, revision: u64) -> Option<Option<GroupingEvidence>> {
        self.get_with_inference(source, revision, false)
    }

    fn get_with_inference(
        &self,
        source: &SourceIdentity,
        revision: u64,
        allow_inferred: bool,
    ) -> Option<Option<GroupingEvidence>> {
        let entries = self.snapshot().entries;
        let confirmed = entries
            .get(&key(source, revision))
            .filter(|entry| entry.live());
        if confirmed.is_some_and(|entry| entry.evidence.is_some()) || !allow_inferred {
            return confirmed.map(|entry| entry.evidence.clone());
        }
        // An off-policy negative says nothing about an optional inference.
        entries
            .get(&inference_key(source, revision))
            .filter(|entry| entry.live())
            .map(|entry| entry.evidence.clone())
    }
}

/// None = not cached; Some(None) = a recent unconfirmed/failed lookup.
pub fn cached_evidence(source: &SourceIdentity, revision: u64) -> Option<Option<GroupingEvidence>> {
    store().get(source, revision)
}

pub fn cached_evidence_with_inference(
    source: &SourceIdentity,
    revision: u64,
    allow_inferred: bool,
) -> Option<Option<GroupingEvidence>> {
    store().get_with_inference(source, revision, allow_inferred)
}

pub(crate) fn remember_inferred_evidence(
    source: &SourceIdentity,
    revision: u64,
    evidence: Option<GroupingEvidence>,
) {
    store().remember_with_policy(source, revision, evidence, true);
}

pub fn remember_evidence(
    source: &SourceIdentity,
    revision: u64,
    evidence: Option<GroupingEvidence>,
) {
    store().remember(source, revision, evidence);
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

    #[derive(Default)]
    struct BlockingDisk {
        value: Mutex<Option<String>>,
        gate: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
        fail: std::sync::atomic::AtomicBool,
    }
    impl MetadataCache for BlockingDisk {
        fn load(&self) -> Result<Option<String>, ProviderHostError> {
            Ok(self.value.lock().unwrap().clone())
        }
        fn save(&self, value: &str) -> Result<(), ProviderHostError> {
            let gate = self.gate.lock().unwrap().take();
            if let Some((started, release)) = gate {
                let _ = started.send(());
                let _ = release.recv();
            }
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(ProviderHostError("fixture write failed".into()));
            }
            *self.value.lock().unwrap() = Some(value.into());
            Ok(())
        }
    }

    #[test]
    fn a_blocked_durable_write_never_blocks_snapshot_reads_or_publishes_early() {
        let disk = Arc::new(BlockingDisk::default());
        let cache = Arc::new(Cache::load(disk.clone()).unwrap());
        let first = source();
        cache.remember(&first, 7, Some(evidence(&first)));
        let mut second = first.clone();
        second.id = "mal:2".into();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *disk.gate.lock().unwrap() = Some((started_tx, release_rx));
        let worker = {
            let cache = cache.clone();
            let second = second.clone();
            std::thread::spawn(move || cache.remember(&second, 7, Some(evidence(&second))))
        };
        started_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        let (read_tx, read_rx) = std::sync::mpsc::channel();
        let reader = {
            let cache = cache.clone();
            let first = first.clone();
            let second = second.clone();
            std::thread::spawn(move || {
                read_tx
                    .send((cache.get(&first, 7), cache.get(&second, 7)))
                    .unwrap();
            })
        };
        let before = read_rx.recv_timeout(std::time::Duration::from_secs(3));
        // Always release the writer before asserting, including a regression
        // where the reader timed out behind the persistence lock.
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        reader.join().unwrap();
        let (existing, pending) =
            before.expect("cache reads must finish while durable save is blocked");
        assert!(existing.flatten().is_some());
        assert!(pending.is_none());
        assert!(cache.get(&second, 7).flatten().is_some());
        disk.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut failed = first.clone();
        failed.id = "mal:failed".into();
        cache.remember(&failed, 7, Some(evidence(&failed)));
        assert!(cache.get(&failed, 7).is_none());
        assert!(cache.get(&first, 7).flatten().is_some());
    }

    #[test]
    fn concurrent_writers_preserve_both_updates_and_backend_replacements_are_serialized() {
        let disk = Arc::new(Disk::default());
        let cache = Arc::new(Cache::load(disk.clone()).unwrap());
        let first = source();
        let mut second = first.clone();
        second.id = "mal:2".into();
        let start = Arc::new(std::sync::Barrier::new(3));
        let workers = [first.clone(), second.clone()].map(|source| {
            let cache = cache.clone();
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                cache.remember(&source, 7, Some(evidence(&source)));
            })
        });
        start.wait();
        for worker in workers {
            worker.join().unwrap();
        }
        let restored = Cache::load(disk).unwrap();
        assert!(restored.get(&first, 7).flatten().is_some());
        assert!(restored.get(&second, 7).flatten().is_some());
        let replacement = Arc::new(Disk::default());
        cache.replace_backend(replacement.clone());
        assert!(cache.get(&first, 7).is_none());
        cache.remember(&first, 8, Some(evidence(&first)));
        assert!(
            Cache::load(replacement)
                .unwrap()
                .get(&first, 8)
                .flatten()
                .is_some()
        );
    }

    #[test]
    fn durable_evidence_keeps_owner_and_inventory_revisions_separate() {
        let disk = Arc::new(Disk::default());
        let cache = Cache::load(disk.clone()).unwrap();
        let first = source();
        cache.remember(&first, 7, Some(evidence(&first)));
        let mut other = first.clone();
        other.addon_url = "https://other.example/secret".into();
        cache.remember(&other, 7, None);
        cache.remember(&first, 8, None);
        let restored = Cache::load(disk).unwrap();
        assert!(
            restored.snapshot().entries[&key(&first, 7)]
                .evidence
                .is_some()
        );
        assert!(
            restored.snapshot().entries[&key(&other, 7)]
                .evidence
                .is_none()
        );
        assert!(
            restored.snapshot().entries[&key(&first, 8)]
                .evidence
                .is_none()
        );
    }

    #[test]
    fn inferred_results_and_negatives_are_policy_scoped_and_confirmed_proof_wins() {
        let disk = Arc::new(Disk::default());
        let cache = Cache::load(disk.clone()).unwrap();
        let source = source();
        cache.remember(&source, 7, None);
        assert_eq!(cache.get(&source, 7), Some(None));
        assert_eq!(
            cache.get_with_inference(&source, 7, true),
            None,
            "off-policy negative cannot suppress inference"
        );
        cache.remember_with_policy(&source, 7, None, true);
        assert_eq!(cache.get_with_inference(&source, 7, true), Some(None));
        let mut inferred = evidence(&source);
        inferred.season = SeasonMembership::Unknown;
        inferred.source_inventory_fingerprint =
            Some(super::super::episode_inventory_fingerprint(["kitsu:3:1"]));
        inferred.proofs = vec![GroupingProof::InferredSeasonChain {
            provider_id: crate::addon_metadata_id(&source.addon_url),
            related_release_id: "kitsu:2".into(),
            source_season: 3,
            related_season: 2,
            parent_preview: Box::new(super::super::InferredParentPreview {
                source: SourceIdentity {
                    addon_url: source.addon_url.clone(),
                    media_type: "series".into(),
                    id: "tt1".into(),
                },
                name: "Show".into(),
                poster: None,
                background: None,
                logo: None,
                release_info: None,
            }),
        }];
        cache.remember(&source, 7, Some(inferred.clone()));
        assert_eq!(
            cache.get(&source, 7),
            Some(None),
            "Detail/default getter never sees inferred proof"
        );
        assert_eq!(
            cache.get_with_inference(&source, 7, true),
            Some(Some(inferred.clone()))
        );
        let restarted = Cache::load(disk).unwrap();
        assert_eq!(restarted.get(&source, 7), Some(None));
        assert_eq!(
            restarted.get_with_inference(&source, 7, true),
            Some(Some(inferred))
        );
        let confirmed = evidence(&source);
        restarted.remember(&source, 7, Some(confirmed.clone()));
        assert_eq!(
            restarted.get_with_inference(&source, 7, true),
            Some(Some(confirmed))
        );
    }

    #[test]
    fn cache_is_bounded_negative_entries_expire_and_corrupt_storage_is_preserved() {
        let disk = Arc::new(Disk::default());
        let cache = Cache::load(disk.clone()).unwrap();
        for i in 0..COUNT_LIMIT + 3 {
            let mut source = source();
            source.id = format!("mal:{i}");
            cache.remember(&source, 7, None);
        }
        assert_eq!(cache.snapshot().entries.len(), COUNT_LIMIT);
        let mut old = Entry {
            version: CACHE_VERSION,
            revision: 7,
            at: now().saturating_sub(NEGATIVE_TTL + 1),
            source: source(),
            evidence: None,
            inferred_policy: false,
        };
        assert!(!old.valid());
        old.evidence = Some(evidence(&old.source));
        assert!(old.valid());
        disk.save("not-json").unwrap();
        assert!(Cache::load(disk.clone()).is_none());
        assert_eq!(disk.load().unwrap().as_deref(), Some("not-json"));
    }
}

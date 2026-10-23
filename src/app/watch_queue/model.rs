//! Queue membership is independent of the last-writer-wins order and cursor.
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub(super) struct Cursor {
    pub item_key: Option<String>,
    #[serde(default)]
    pub phase: Phase,
    pub last_completed_key: Option<String>,
    #[serde(default)]
    pub origin_device: u64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    #[default]
    Ready,
    InProgress,
    Completed,
    Finished,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SavedQueue {
    pub version: u32,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub order: Vec<String>,
    #[serde(default)]
    pub cursor: Cursor,
    #[serde(default)]
    pub autoplay: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}
impl Default for SavedQueue {
    fn default() -> Self {
        Self {
            version: 2,
            entries: vec![],
            order: vec![],
            cursor: Cursor::default(),
            autoplay: false,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Order {
    keys: Vec<String>,
    #[serde(default)]
    autoplay: bool,
}

pub(super) fn decode(raw: &str) -> Result<SavedQueue, String> {
    if raw.len() > 8 * 1024 * 1024 {
        return Err("queue data too large".into());
    }
    let mut saved: SavedQueue = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    if !(1..=2).contains(&saved.version)
        || saved.entries.len() > 10_000
        || (saved.version == 1 && saved.entries.len() > MAX_ITEMS)
    {
        return Err("unsupported queue data".into());
    }
    if saved.entries.iter().any(|entry| {
        [entry.type_.as_str(), &entry.series_id, &entry.episode_id]
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 1024)
    }) {
        return Err("invalid queue identity".into());
    }
    if saved.version == 1 {
        saved.order = saved.entries.iter().map(Entry::key).collect();
    }
    saved.version = 2;
    materialize(&mut saved);
    Ok(saved)
}

pub(super) fn materialize(saved: &mut SavedQueue) {
    let mut live: BTreeMap<_, _> = saved.entries.drain(..).map(|e| (e.key(), e)).collect();
    for key in &saved.order {
        if let Some(entry) = live.remove(key) {
            saved.entries.push(entry);
        }
    }
    let mut appended: Vec<_> = live.into_values().collect();
    appended.sort_by_key(|e| (e.created_ms, e.created_device, e.key()));
    saved.entries.extend(appended);
    if saved
        .cursor
        .item_key
        .as_ref()
        .is_some_and(|key| !saved.entries.iter().any(|e| e.key() == *key))
    {
        // Resolve from the saved context only. A missing context must not jump
        // to an arbitrary newly received row.
        let next = saved
            .cursor
            .item_key
            .as_ref()
            .and_then(|key| saved.order.iter().position(|k| k == key))
            .and_then(|i| {
                saved.order[i + 1..]
                    .iter()
                    .find(|key| saved.entries.iter().any(|e| e.key() == **key))
            })
            .cloned();
        saved.cursor.item_key = next;
        saved.cursor.phase = if saved.cursor.item_key.is_some() {
            Phase::Ready
        } else {
            Phase::Finished
        };
    }
}

pub(crate) fn records(raw: &str) -> nova_sync::Result<Vec<sync::SnapshotDomainRecords>> {
    let saved =
        decode(raw).map_err(|error| storage::Error::new(storage::ErrorKind::Schema, error))?;
    Ok(vec![
        (
            ITEMS,
            saved
                .entries
                .iter()
                .map(|entry| Ok((entry.key(), serde_json::to_string(entry)?, 0)))
                .collect::<Result<_, serde_json::Error>>()?,
        ),
        (
            ORDER,
            vec![(
                "order".into(),
                serde_json::to_string(&Order {
                    keys: saved.order,
                    autoplay: saved.autoplay,
                })?,
                0,
            )],
        ),
        (
            CURSOR,
            vec![("cursor".into(), serde_json::to_string(&saved.cursor)?, 0)],
        ),
    ])
}

pub(crate) fn snapshot(
    store: &nova_sync::Store,
    owner: &profiles::ProfileStore,
) -> nova_sync::Result<String> {
    let mut saved = project(store, owner)?;
    if let Some(raw) = owner.read(KEY)? {
        saved.extra = decode(&raw)
            .map_err(|error| storage::Error::new(storage::ErrorKind::Schema, error))?
            .extra;
    }
    Ok(serde_json::to_string(&saved)?)
}

fn project(
    store: &nova_sync::Store,
    owner: &profiles::ProfileStore,
) -> nova_sync::Result<SavedQueue> {
    let mut saved = SavedQueue {
        entries: store
            .records(&owner.domain(ITEMS))
            .into_iter()
            .map(|(key, raw)| {
                let entry: Entry = serde_json::from_str(&raw)?;
                if entry.key() != key {
                    return Err(storage::Error::new(
                        storage::ErrorKind::Schema,
                        "queue record identity mismatch",
                    )
                    .into());
                }
                Ok(entry)
            })
            .collect::<nova_sync::Result<_>>()?,
        ..SavedQueue::default()
    };
    if let Some((_, raw)) = store
        .records(&owner.domain(ORDER))
        .into_iter()
        .find(|(key, _)| key == "order")
    {
        let order: Order = serde_json::from_str(&raw)?;
        saved.order = order.keys;
        saved.autoplay = order.autoplay;
    }
    if let Some((_, raw)) = store
        .records(&owner.domain(CURSOR))
        .into_iter()
        .find(|(key, _)| key == "cursor")
    {
        saved.cursor = serde_json::from_str(&raw)?;
    }
    materialize(&mut saved);
    Ok(saved)
}

pub(crate) fn portable_merge(
    local: &str,
    backup: &str,
    use_backup: bool,
) -> Result<String, String> {
    let mut local = decode(local)?;
    let backup = decode(backup)?;
    local.order = local.entries.iter().map(Entry::key).collect();
    if use_backup {
        local.cursor = backup.cursor.clone();
        local.autoplay = backup.autoplay;
        for entry in &backup.entries {
            if let Some(existing) = local
                .entries
                .iter_mut()
                .find(|existing| existing.key() == entry.key())
            {
                *existing = entry.clone();
            }
        }
    }
    let mut keys: HashSet<_> = local.entries.iter().map(Entry::key).collect();
    for entry in backup.entries {
        if keys.insert(entry.key()) {
            local.order.push(entry.key());
            local.entries.push(entry);
        }
    }
    serde_json::to_string(&local).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, created_ms: u64) -> Entry {
        Entry {
            type_: "series".into(),
            series_id: "show".into(),
            episode_id: id.into(),
            title: id.into(),
            poster_url: String::new(),
            episode_title: String::new(),
            season: Some(1),
            episode: Some(1),
            released: None,
            created_ms,
            created_device: 1,
            source_preference: None,
            extra: BTreeMap::new(),
        }
    }
    fn keys(saved: &SavedQueue) -> Vec<String> {
        saved.entries.iter().map(|e| e.episode_id.clone()).collect()
    }

    #[test]
    fn losing_order_preserves_concurrent_membership_and_tombstones_cannot_resurrect() {
        let owner = profiles::ProfileStore::for_profile("queue-test");
        let a = entry("a", 1);
        let b = entry("b", 2);
        let c = entry("c", 3);
        let mut left = nova_sync::Store::default();
        for item in [&a, &b] {
            left.set(
                &owner.domain(ITEMS),
                &item.key(),
                Some(serde_json::to_string(item).unwrap()),
                0,
                1,
            );
        }
        left.set(
            &owner.domain(ORDER),
            "order",
            Some(
                serde_json::to_string(&Order {
                    keys: vec![b.key(), a.key()],
                    autoplay: false,
                })
                .unwrap(),
            ),
            0,
            1,
        );
        let mut right = left.clone();
        left.set(
            &owner.domain(ITEMS),
            &c.key(),
            Some(serde_json::to_string(&c).unwrap()),
            0,
            1,
        );
        right.set(&owner.domain(ITEMS), &a.key(), None, 0, 2);
        right.set(
            &owner.domain(ORDER),
            "order",
            Some(
                serde_json::to_string(&Order {
                    keys: vec![a.key(), b.key(), b.key()],
                    autoplay: true,
                })
                .unwrap(),
            ),
            0,
            2,
        );
        let domains = [ITEMS, ORDER];
        for domain in domains {
            let domain = owner.domain(domain);
            let mut all: BTreeMap<_, _> = left
                .digest()
                .get(&domain)
                .unwrap()
                .iter()
                .map(|(key, _)| (key.clone(), left.record(&domain, key).unwrap().clone()))
                .collect();
            for (key, _) in right.digest().get(&domain).unwrap() {
                let record = right.record(&domain, key).unwrap().clone();
                if all
                    .get(key)
                    .is_none_or(|old| record.version.newer_than(&old.version))
                {
                    all.insert(key.clone(), record);
                }
            }
            for (key, record) in all {
                left.apply(&domain, &key, record.clone());
                right.apply(&domain, &key, record);
            }
        }
        assert_eq!(left.digest(), right.digest());
        let result = project(&left, &owner).unwrap();
        assert_eq!(keys(&result), ["b", "c"]);
        assert!(result.autoplay);
        assert_eq!(keys(&project(&right, &owner).unwrap()), ["b", "c"]);
    }

    #[test]
    fn migration_preserves_v1_order_and_v2_accepts_over_cap_merged_membership() {
        let original = SavedQueue {
            version: 1,
            entries: vec![entry("b", 0), entry("a", 0)],
            ..Default::default()
        };
        let migrated = decode(&serde_json::to_string(&original).unwrap()).unwrap();
        assert_eq!(migrated.version, 2);
        assert_eq!(keys(&migrated), ["b", "a"]);
        let merged = SavedQueue {
            entries: (0..MAX_ITEMS + 1)
                .map(|i| entry(&i.to_string(), i as u64))
                .collect(),
            ..Default::default()
        };
        assert_eq!(
            decode(&serde_json::to_string(&merged).unwrap())
                .unwrap()
                .entries
                .len(),
            MAX_ITEMS + 1
        );
    }

    #[test]
    fn removed_cursor_uses_saved_context_and_unknown_fields_survive_merge() {
        let a = entry("a", 1);
        let b = entry("b", 2);
        let c = entry("c", 3);
        let mut saved = SavedQueue {
            entries: vec![c.clone(), b.clone()],
            order: vec![a.key(), b.key(), c.key()],
            cursor: Cursor {
                item_key: Some(a.key()),
                ..Default::default()
            },
            ..Default::default()
        };
        saved
            .extra
            .insert("future".into(), serde_json::json!({"keep":true}));
        materialize(&mut saved);
        assert_eq!(saved.cursor.item_key, Some(b.key()));
        assert_eq!(saved.cursor.phase, Phase::Ready);
        let backup = SavedQueue {
            entries: vec![a],
            ..Default::default()
        };
        let result = decode(
            &portable_merge(
                &serde_json::to_string(&saved).unwrap(),
                &serde_json::to_string(&backup).unwrap(),
                true,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(keys(&result), ["b", "c", "a"]);
        assert!(result.extra.contains_key("future"));
    }

    #[test]
    fn v1_migration_commits_snapshot_records_and_baselines_once() {
        const ROOT: &str = "NOVA_QUEUE_MIGRATION_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-queue-migration-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output=std::process::Command::new(std::env::current_exe().unwrap()).env(ROOT,&root).args(["--exact","app::watch_queue::model::tests::v1_migration_commits_snapshot_records_and_baselines_once","--nocapture"]).output().unwrap();
            let _ = std::fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        nova_storage::init_at(&std::path::PathBuf::from(root));
        let owner = profiles::ProfileStore::for_profile("queue-migration");
        let raw = serde_json::to_string(&SavedQueue {
            version: 1,
            entries: vec![entry("b", 0), entry("a", 0)],
            ..Default::default()
        })
        .unwrap();
        owner.write("watch_queue:v1", &raw).unwrap();
        let saved = super::super::load(&owner).unwrap();
        assert_eq!(keys(&saved), ["b", "a"]);
        assert_eq!(
            owner.read("watch_queue:v1").unwrap().as_deref(),
            Some(raw.as_str())
        );
        let store = nova_sync::local_store().unwrap();
        let before = store.lock().unwrap().digest();
        assert_eq!(store.lock().unwrap().records(&owner.domain(ITEMS)).len(), 2);
        for domain in [ITEMS, ORDER, CURSOR] {
            assert!(
                nova_storage::try_get_str(&format!("sync:baseline:{}", owner.domain(domain)))
                    .unwrap()
                    .is_some()
            );
        }
        assert_eq!(
            decode(&owner.read(KEY).unwrap().unwrap()).unwrap().version,
            2
        );
        super::super::load(&owner).unwrap();
        assert_eq!(
            store.lock().unwrap().digest(),
            before,
            "migration never restamps an existing v2 queue"
        );
    }
}

//! Socket-free mutation owner. App snapshots and their materialized baselines
//! commit atomically with changed record rows in the same redb transaction.
use crate::Store;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

static LOCAL: Mutex<Option<Arc<Mutex<Store>>>> = Mutex::new(None);

pub fn local_store() -> Result<Arc<Mutex<Store>>> {
    let mut slot = LOCAL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(store) = slot.as_ref() {
        return Ok(store.clone());
    }
    let mut store = Store::try_load()?;
    store.set_device(local_device()?);
    let store = Arc::new(Mutex::new(store));
    *slot = Some(store.clone());
    Ok(store)
}

pub fn local_device() -> Result<u64> {
    Ok(nova_config::fnv1a(
        crate::load_or_create_secret()?.public().as_bytes(),
    ))
}

/// Missing keys only imply deletion if present in our materialized baseline,
/// never merely because a remote update exists in the record owner.
pub fn commit_snapshot(
    domain: &str,
    records: &[(String, String, u64)],
    snapshot: Option<(&str, String)>,
    projection: bool,
    seed: bool,
) -> Result<()> {
    let owner = local_store()?;
    let dev = local_device()?;
    let mut store = owner.lock().unwrap_or_else(|e| e.into_inner());
    let changed = prepare_snapshot(&mut store, dev, domain, records, snapshot, projection, seed)?;
    store.save()?;
    drop(store);
    if changed && !projection && !seed {
        crate::request_local_sync();
    }
    Ok(())
}

pub fn prepare_snapshot(
    store: &mut Store,
    dev: u64,
    domain: &str,
    records: &[(String, String, u64)],
    snapshot: Option<(&str, String)>,
    projection: bool,
    seed: bool,
) -> Result<bool> {
    let mut changed = false;
    let baseline_key = format!("sync:baseline:{domain}");
    let baseline: BTreeMap<String, String> = store
        .extra_value(&baseline_key)?
        .map(|raw| serde_json::from_str(&raw))
        .transpose()
        .context("materialized baseline")?
        .unwrap_or_default();
    let current: BTreeMap<String, String> = records
        .iter()
        .map(|(k, v, _)| (k.clone(), v.clone()))
        .collect();
    if !projection {
        for key in baseline.keys() {
            if !current.contains_key(key) && !seed {
                changed |= store.set(domain, key, None, 0, dev);
            }
        }
        for (key, value, ts) in records {
            let differs = baseline.get(key) != Some(value);
            if (seed && store.record(domain, key).is_none()) || (!seed && differs) {
                let value = preserve_unknown(
                    store.record(domain, key).and_then(|r| r.value.as_deref()),
                    value,
                );
                changed |= store.set(domain, key, Some(value), *ts, dev);
            }
        }
    }
    store.queue_extra(&baseline_key, Some(serde_json::to_string(&current)?));
    if let Some((key, raw)) = snapshot {
        store.queue_extra(key, Some(raw));
    }
    Ok(changed)
}

/// Typed clients may update known object fields without erasing newer fields.
pub fn preserve_unknown(old: Option<&str>, new: &str) -> String {
    let old = old.and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok());
    let new_value = serde_json::from_str::<serde_json::Value>(new).ok();
    match (old, new_value) {
        (Some(serde_json::Value::Object(mut old)), Some(serde_json::Value::Object(new))) => {
            old.extend(new);
            serde_json::Value::Object(old).to_string()
        }
        _ => new.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_noops_and_remote_projections_do_not_request_publication() {
        let mut store = Store::default();
        store.queue_extra("sync:baseline:library", Some("{}".into()));
        let records = vec![("a".into(), r#"{"title":"a"}"#.into(), 0)];
        assert!(prepare_snapshot(&mut store, 1, "library", &records, None, false, false).unwrap());
        assert!(!prepare_snapshot(&mut store, 1, "library", &records, None, false, false).unwrap());
        let projected = vec![("b".into(), r#"{"title":"remote"}"#.into(), 0)];
        assert!(
            !prepare_snapshot(&mut store, 1, "library", &projected, None, true, false).unwrap()
        );
        assert!(store.record("library", "b").is_none());
        assert!(
            !prepare_snapshot(&mut store, 1, "library", &projected, None, false, false).unwrap()
        );
    }
}

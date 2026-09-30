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
    prepare_snapshot(&mut store, dev, domain, records, snapshot, projection, seed)?;
    store.save()
}

pub fn prepare_snapshot(
    store: &mut Store,
    dev: u64,
    domain: &str,
    records: &[(String, String, u64)],
    snapshot: Option<(&str, String)>,
    projection: bool,
    seed: bool,
) -> Result<()> {
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
                store.set(domain, key, None, 0, dev);
            }
        }
        for (key, value, ts) in records {
            let changed = baseline.get(key) != Some(value);
            if (seed && store.record(domain, key).is_none()) || (!seed && changed) {
                let value = preserve_unknown(
                    store.record(domain, key).and_then(|r| r.value.as_deref()),
                    value,
                );
                store.set(domain, key, Some(value), *ts, dev);
            }
        }
    }
    store.queue_extra(&baseline_key, Some(serde_json::to_string(&current)?));
    if let Some((key, raw)) = snapshot {
        store.queue_extra(key, Some(raw));
    }
    Ok(())
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

//! Persistent key-value storage backed by a single redb database at
//! `<data_dir>/nova.redb`.
//!
//! Durable reads/writes work from any thread (redb serializes writers
//! internally). The database is opened lazily on first use; a failed open
//! (second instance, corrupt file) degrades gracefully to a no-op backend.

use redb::{Database, ReadableDatabase, TableDefinition};
use std::path::Path;
use std::sync::{LazyLock, OnceLock};

const KV: TableDefinition<&str, &str> = TableDefinition::new("kv");

static DB_PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
static DB: LazyLock<Option<Database>> = LazyLock::new(|| {
    let path = DB_PATH.get()?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match Database::create(path) {
        Ok(db) => Some(db),
        Err(e) => {
            eprintln!("nova storage: could not open nova.redb ({e}); running without persistence");
            None
        }
    }
});

/// Open (or create) the database inside `data_dir`. Call once at startup
/// before any `get_str`/`set_str`.
pub fn init_at(data_dir: &Path) {
    let _ = DB_PATH.set(data_dir.join("nova.redb"));
    LazyLock::force(&DB);
}

pub fn get_str(key: &str) -> Option<String> {
    let db = DB.as_ref()?;
    let txn = db.begin_read().ok()?;
    let table = txn.open_table(KV).ok()?;
    // `.value()` borrows the `AccessGuard`; clone before txn drops.
    table.get(key).ok()?.map(|g| g.value().to_string())
}

pub fn set_str(key: &str, value: &str) {
    let Some(db) = DB.as_ref() else { return };
    let Ok(txn) = db.begin_write() else {
        eprintln!("nova storage: begin_write failed");
        return;
    };
    {
        if let Ok(mut table) = txn.open_table(KV)
            && let Err(e) = table.insert(key, value)
        {
            eprintln!("nova storage: insert({key}): {e}");
        }
    }
    if let Err(e) = txn.commit() {
        eprintln!("nova storage: commit({key}): {e}");
    }
}

pub fn remove(key: &str) {
    let Some(db) = DB.as_ref() else { return };
    let Ok(txn) = db.begin_write() else { return };
    {
        if let Ok(mut table) = txn.open_table(KV) {
            let _ = table.remove(key);
        }
    }
    let _ = txn.commit();
}

/// Insert/remove many keys in a single transaction (`None` removes the key).
/// Batch writes let a subsystem with per-record rows persist a whole dirty set
/// without paying one transaction per record.
pub fn write_batch(entries: &[(String, Option<String>)]) {
    let Some(db) = DB.as_ref() else { return };
    let Ok(txn) = db.begin_write() else {
        eprintln!("nova storage: begin_write failed");
        return;
    };
    {
        if let Ok(mut table) = txn.open_table(KV) {
            for (key, value) in entries {
                let result = match value {
                    Some(value) => table.insert(key.as_str(), value.as_str()).map(|_| ()),
                    None => table.remove(key.as_str()).map(|_| ()),
                };
                if let Err(e) = result {
                    eprintln!("nova storage: write_batch({key}): {e}");
                }
            }
        }
    }
    if let Err(e) = txn.commit() {
        eprintln!("nova storage: commit batch: {e}");
    }
}

/// All `(key, value)` pairs whose key starts with `prefix`, in key order.
pub fn scan_prefix(prefix: &str) -> Vec<(String, String)> {
    let Some(db) = DB.as_ref() else { return Vec::new() };
    let Ok(txn) = db.begin_read() else { return Vec::new() };
    let Ok(table) = txn.open_table(KV) else {
        return Vec::new();
    };
    let Ok(range) = table.range(prefix..) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in range.flatten() {
        let (key, value) = entry;
        let key = key.value();
        if !key.starts_with(prefix) {
            break;
        }
        out.push((key.to_string(), value.value().to_string()));
    }
    out
}

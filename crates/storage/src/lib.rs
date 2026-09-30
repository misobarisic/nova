//! Persistent key-value storage backed by a single redb database at
//! `<data_dir>/nova.redb`.
//!
//! Durable reads/writes work from any thread (redb serializes writers
//! internally). Startup opens the database explicitly. Fallible APIs preserve
//! missing-vs-failed outcomes; convenience wrappers record diagnostic failures.

use redb::{Database, ReadableDatabase, TableDefinition};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

const KV: TableDefinition<&str, &str> = TableDefinition::new("kv");

static DB_PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
static DB: OnceLock<Result<Database, Error>> = OnceLock::new();
static LAST_ERROR: Mutex<Option<Error>> = Mutex::new(None);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Unavailable,
    Transaction,
    Schema,
    Serialization,
}

#[derive(Clone, Debug)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}
impl Error {
    pub fn new(kind: ErrorKind, message: impl ToString) -> Self {
        Self {
            kind,
            message: message.to_string(),
        }
    }
}
pub fn report(error: Error) {
    eprintln!("nova storage: {error}");
    *LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
}
pub fn last_error() -> Option<Error> {
    LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone()
}
fn database() -> Result<&'static Database, Error> {
    DB.get()
        .ok_or_else(|| Error::new(ErrorKind::Unavailable, "storage not initialized"))?
        .as_ref()
        .map_err(Clone::clone)
}
fn transaction(e: impl ToString) -> Error {
    Error::new(ErrorKind::Transaction, e)
}

/// Open (or create) the database inside `data_dir`. Call once at startup
/// before any `get_str`/`set_str`.
pub fn init_at(data_dir: &Path) {
    let _ = DB_PATH.set(data_dir.join("nova.redb"));
    let result = DB.get_or_init(|| {
        let path = DB_PATH
            .get()
            .ok_or_else(|| Error::new(ErrorKind::Unavailable, "missing database path"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::new(ErrorKind::Unavailable, e))?;
        }
        Database::create(path).map_err(|e| Error::new(ErrorKind::Unavailable, e))
    });
    if let Err(e) = result {
        report(e.clone());
    }
}

pub fn get_str(key: &str) -> Option<String> {
    match try_get_str(key) {
        Ok(v) => v,
        Err(e) => {
            report(e);
            None
        }
    }
}

pub fn try_get_str(key: &str) -> Result<Option<String>, Error> {
    let txn = database()?.begin_read().map_err(transaction)?;
    let table = match txn.open_table(KV) {
        Ok(table) => table,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(e) => return Err(transaction(e)),
    };
    Ok(table
        .get(key)
        .map_err(transaction)?
        .map(|g| g.value().to_string()))
}

pub fn set_str(key: &str, value: &str) {
    if let Err(e) = try_set_str(key, value) {
        report(e);
    }
}
pub fn try_set_str(key: &str, value: &str) -> Result<(), Error> {
    try_write_batch(&[(key.to_string(), Some(value.to_string()))])
}

pub fn remove(key: &str) {
    if let Err(e) = try_write_batch(&[(key.to_string(), None)]) {
        report(e);
    }
}

/// Insert/remove many keys in a single transaction (`None` removes the key).
/// Batch writes let a subsystem with per-record rows persist a whole dirty set
/// without paying one transaction per record.
pub fn write_batch(entries: &[(String, Option<String>)]) {
    if let Err(e) = try_write_batch(entries) {
        report(e);
    }
}
pub fn try_write_batch(entries: &[(String, Option<String>)]) -> Result<(), Error> {
    let txn = database()?.begin_write().map_err(transaction)?;
    {
        let mut table = txn.open_table(KV).map_err(transaction)?;
        for (key, value) in entries {
            let result = match value {
                Some(value) => table.insert(key.as_str(), value.as_str()).map(|_| ()),
                None => table.remove(key.as_str()).map(|_| ()),
            };
            result.map_err(transaction)?;
        }
    }
    txn.commit().map_err(transaction)?;
    let mut error = LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner());
    if error
        .as_ref()
        .is_some_and(|e| e.kind == ErrorKind::Transaction)
    {
        *error = None;
    }
    Ok(())
}

/// All `(key, value)` pairs whose key starts with `prefix`, in key order.
pub fn scan_prefix(prefix: &str) -> Vec<(String, String)> {
    match try_scan_prefix(prefix) {
        Ok(v) => v,
        Err(e) => {
            report(e);
            Vec::new()
        }
    }
}
pub fn try_scan_prefix(prefix: &str) -> Result<Vec<(String, String)>, Error> {
    let txn = database()?.begin_read().map_err(transaction)?;
    let table = match txn.open_table(KV) {
        Ok(table) => table,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
        Err(e) => return Err(transaction(e)),
    };
    let range = table.range(prefix..).map_err(transaction)?;
    let mut out = Vec::new();
    for entry in range {
        let (key, value) = entry.map_err(transaction)?;
        let key = key.value();
        if !key.starts_with(prefix) {
            break;
        }
        out.push((key.to_string(), value.value().to_string()));
    }
    Ok(out)
}

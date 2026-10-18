//! Logical app storage. Account keys and already qualified keys pass through.
pub use nova_storage::{CollectionKind, Error, ErrorKind, init_at, last_error, report};
pub fn try_get_str(name: &str) -> Result<Option<String>, Error> {
    super::ProfileStore::capture().read(name)
}
pub fn get_str(name: &str) -> Option<String> {
    match try_get_str(name) {
        Ok(v) => v,
        Err(e) => {
            report(e);
            None
        }
    }
}
pub fn try_set_str(name: &str, value: &str) -> Result<(), Error> {
    super::ProfileStore::capture().write(name, value)
}
pub fn set_str(name: &str, value: &str) {
    if let Err(e) = try_set_str(name, value) {
        report(e);
    }
}
pub fn try_set_cached_str(name: &str, value: &str) -> Result<(), Error> {
    super::ProfileStore::capture().write_cached(name, value)
}
pub fn remove(name: &str) {
    if let Err(e) = try_write_batch(&[(name.into(), None)]) {
        report(e);
    }
}
pub fn try_write_batch(entries: &[(String, Option<String>)]) -> Result<(), Error> {
    super::ProfileStore::capture().write_batch(entries)
}
pub fn try_scan_prefix(prefix: &str) -> Result<Vec<(String, String)>, Error> {
    super::ProfileStore::capture().scan(prefix)
}
#[cfg(test)]
pub fn scan_prefix(prefix: &str) -> Vec<(String, String)> {
    match try_scan_prefix(prefix) {
        Ok(v) => v,
        Err(e) => {
            report(e);
            Vec::new()
        }
    }
}

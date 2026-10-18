//! Logical app storage. Account keys and already qualified keys pass through.
pub use nova_storage::{CollectionKind, Error, ErrorKind, init_at, last_error, report};
fn key(key: &str) -> String {
    super::storage_key(key)
}
fn check(key: &str) -> Result<(), Error> {
    let id = super::parse_domain(key)
        .map(|(id, _)| id.to_string())
        .or_else(|| super::scoped_key(key).then(super::current_id));
    if id.is_some_and(|id| !super::writable(&id)) {
        return Err(Error::new(
            ErrorKind::Transaction,
            "profile was deleted; late write discarded",
        ));
    }
    Ok(())
}
pub fn try_get_str(name: &str) -> Result<Option<String>, Error> {
    nova_storage::try_get_str(&key(name))
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
    try_write_batch(&[(name.into(), Some(value.into()))])
}
pub fn set_str(name: &str, value: &str) {
    if let Err(e) = try_set_str(name, value) {
        report(e);
    }
}
pub fn try_set_cached_str(name: &str, value: &str) -> Result<(), Error> {
    let _serial = super::WRITE_SERIAL.lock().unwrap();
    check(name)?;
    nova_storage::try_set_cached_str(&key(name), value)
}
pub fn remove(name: &str) {
    if let Err(e) = try_write_batch(&[(name.into(), None)]) {
        report(e);
    }
}
pub fn try_write_batch(entries: &[(String, Option<String>)]) -> Result<(), Error> {
    let _serial = super::WRITE_SERIAL.lock().unwrap();
    let mut mapped = Vec::with_capacity(entries.len());
    for (name, value) in entries {
        check(name)?;
        mapped.push((key(name), value.clone()));
    }
    nova_storage::try_write_batch(&mapped)
}
pub fn try_scan_prefix(prefix: &str) -> Result<Vec<(String, String)>, Error> {
    let mapped = key(prefix);
    let namespaced = mapped != prefix;
    let rows = nova_storage::try_scan_prefix(&mapped)?;
    Ok(rows
        .into_iter()
        .map(|(name, value)| {
            let name = if namespaced {
                super::parse_domain(&name)
                    .map(|(_, logical)| logical.to_string())
                    .unwrap_or(name)
            } else {
                name
            };
            (name, value)
        })
        .collect())
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
pub fn register_collection(name: &str, kind: CollectionKind) {
    nova_storage::register_collection(&key(name), kind);
}

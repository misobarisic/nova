//! A durable data owner. Selection epochs and UI sessions never enter this type.
use std::cell::RefCell;

use nova_storage::{CollectionKind, Error, ErrorKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProfileStore {
    id: String,
    scoped: bool,
}

thread_local! {
    // Only the existing snapshot decomposition adapter needs ambient ownership.
    static COMPAT_OWNER: RefCell<Option<ProfileStore>> = const { RefCell::new(None) };
}

impl ProfileStore {
    pub(crate) fn capture() -> Self {
        COMPAT_OWNER
            .with(|owner| owner.borrow().clone())
            .unwrap_or_else(|| {
                let context = super::capture();
                context
                    .session
                    .as_ref()
                    .map(|session| session.store.clone())
                    .unwrap_or_else(|| Self {
                        id: context.id,
                        scoped: super::is_initialized(),
                    })
            })
    }

    pub(crate) fn for_profile(id: &str) -> Self {
        assert!(super::valid_id(id));
        Self {
            id: id.into(),
            scoped: true,
        }
    }

    pub(crate) fn is_scoped(&self) -> bool {
        self.scoped
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn key(&self, logical: &str) -> String {
        if self.scoped && super::scoped_key(logical) {
            super::storage_key_for(self.id(), logical)
        } else {
            logical.into()
        }
    }

    pub(crate) fn domain(&self, logical: &str) -> String {
        if self.scoped && super::parse_domain(logical).is_none() {
            super::domain_for(self.id(), logical)
        } else {
            logical.into()
        }
    }

    pub(crate) fn read(&self, logical: &str) -> Result<Option<String>, Error> {
        nova_storage::try_get_str(&self.key(logical))
    }

    pub(crate) fn write(&self, logical: &str, value: &str) -> Result<(), Error> {
        self.write_batch(&[(logical.into(), Some(value.into()))])
    }

    pub(crate) fn write_batch(&self, entries: &[(String, Option<String>)]) -> Result<(), Error> {
        let entries: Vec<_> = entries
            .iter()
            .map(|(key, value)| (self.key(key), value.clone()))
            .collect();
        let _serial = super::WRITE_SERIAL.lock().unwrap();
        for (key, _) in &entries {
            check_key(key)?;
        }
        nova_storage::try_write_batch(&entries)
    }

    pub(crate) fn write_cached(&self, logical: &str, raw: &str) -> Result<(), Error> {
        let key = self.key(logical);
        let _serial = super::WRITE_SERIAL.lock().unwrap();
        check_key(&key)?;
        nova_storage::try_set_cached_str(&key, raw)
    }

    pub(crate) fn scan(&self, prefix: &str) -> Result<Vec<(String, String)>, Error> {
        let mapped = self.key(prefix);
        let scoped = mapped != prefix;
        Ok(nova_storage::try_scan_prefix(&mapped)?
            .into_iter()
            .map(|(key, value)| {
                let key = if scoped {
                    super::parse_domain(&key)
                        .map(|(_, logical)| logical.to_string())
                        .unwrap_or(key)
                } else {
                    key
                };
                (key, value)
            })
            .collect())
    }

    pub(crate) fn register_collection(&self, logical: &str, kind: CollectionKind) {
        nova_storage::register_collection(&self.key(logical), kind);
    }

    pub(crate) fn retired(&self) -> Result<bool, Error> {
        if self.scoped {
            retired_id(&self.id)
        } else {
            Ok(false)
        }
    }

    /// Compatibility for audited synchronous decomposition hooks. The retained
    /// owner contains no session; the temporary context cannot keep UI work alive.
    pub(crate) fn run<R>(&self, f: impl FnOnce() -> R) -> R {
        struct Restore(Option<ProfileStore>);
        impl Drop for Restore {
            fn drop(&mut self) {
                COMPAT_OWNER.with(|owner| *owner.borrow_mut() = self.0.take());
            }
        }
        let old = COMPAT_OWNER.with(|owner| owner.replace(Some(self.clone())));
        let _restore = Restore(old);
        super::with_context(super::context_for(&self.id), f)
    }

    pub(crate) fn persist_snapshot(
        &self,
        key: &str,
        raw: &str,
        seed: bool,
    ) -> nova_sync::Result<()> {
        self.run(|| crate::app::sync::persist_sync_snapshot(key, raw, seed))
    }
}

fn check_key(key: &str) -> Result<(), Error> {
    if let Some((id, _)) = super::parse_domain(key)
        && retired_id(id)?
    {
        return Err(Error::new(
            ErrorKind::Transaction,
            "profile was deleted; late write discarded",
        ));
    }
    Ok(())
}

fn retired_id(id: &str) -> Result<bool, Error> {
    if !super::writable(id) {
        return Ok(true);
    }
    // Read the durable marker directly. Core-store hooks can already hold its
    // mutex when using app storage, so acquiring local_store here would deadlock.
    // WRITE_SERIAL orders local deletion/cleanup with writes; a remote marker
    // committed during a write is followed by cleanup under that same serial lock.
    let domain = super::DELETIONS_DOMAIN;
    let row = format!("srec:{}:{domain}{id}", domain.len());
    nova_storage::try_get_str(&row)?
        .map(|raw| {
            serde_json::from_str::<nova_sync::Record>(&raw)
                .map(|record| !record.is_deleted())
                .map_err(|error| Error::new(ErrorKind::Schema, error))
        })
        .transpose()
        .map(|deleted| deleted.unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_owners_survive_selection_and_enforce_retirement() {
        const ROOT: &str = "NOVA_DURABLE_PROFILE_STORE_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-durable-store-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap()).env(ROOT, &root)
                .args(["--exact", "app::profiles::store::tests::durable_owners_survive_selection_and_enforce_retirement", "--nocapture"])
                .output().unwrap();
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
        let list_keys = [
            "list_services:watched:v1:title",
            "list_services:boundary:v1:account",
            "list_services:simkl_cache:v1:account",
        ];
        for key in list_keys {
            nova_storage::try_set_str(key, "unowned").unwrap();
        }
        nova_storage::try_set_str("library", "legacy library").unwrap();
        nova_storage::try_set_str("profile:default:library", "existing library").unwrap();
        nova_storage::try_set_str("tracking:legacy", "legacy tracking").unwrap();
        nova_storage::try_set_str("library:unrelated", "unrelated").unwrap();
        let entries = super::super::legacy_snapshot_migration_entries().unwrap();
        assert!(entries.contains(&("library".into(), None)));
        assert!(
            !entries
                .iter()
                .any(|(key, _)| key == "profile:default:library")
        );
        assert!(entries.contains(&(
            "profile:default:tracking:legacy".into(),
            Some("legacy tracking".into())
        )));
        assert!(entries.contains(&("tracking:legacy".into(), None)));
        assert!(!entries.iter().any(|(key, _)| key == "library:unrelated"));
        assert!(
            super::super::legacy_snapshot_migration_entries()
                .unwrap()
                .iter()
                .all(|(key, _)| !key.contains("list_services:"))
        );
        let legacy = ProfileStore::capture();
        super::super::initialize().unwrap();
        assert_eq!(legacy.key("library"), "library");
        assert_eq!(legacy.domain("progress"), "progress");
        legacy.run(|| {
            assert_eq!(ProfileStore::capture(), legacy);
            assert_eq!(super::super::storage_key("library"), "library");
            assert_eq!(crate::app::sync::profile_domain("progress"), "progress");
        });
        let alice = ProfileStore::capture();
        alice.write("library", "[1]").unwrap();
        let bob_id = super::super::create("Bob").unwrap();
        let bob =
            super::super::with_context(super::super::context_for(&bob_id), ProfileStore::capture);
        for key in list_keys {
            assert_eq!(alice.read(key).unwrap(), None);
            assert_eq!(bob.read(key).unwrap(), None);
            alice.write(key, "100").unwrap();
            bob.write(key, "10").unwrap();
            assert_eq!(alice.read(key).unwrap().as_deref(), Some("100"));
            assert_eq!(bob.read(key).unwrap().as_deref(), Some("10"));
            assert_eq!(
                nova_storage::try_get_str(key).unwrap().as_deref(),
                Some("unowned")
            );
        }
        assert_eq!(alice.scan("list_services:").unwrap().len(), 3);
        assert!(
            bob.scan("list_services:")
                .unwrap()
                .iter()
                .all(|(_, value)| value == "10")
        );
        let delayed_owner = alice.clone();
        std::thread::spawn(move || delayed_owner.write(list_keys[0], "101"))
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(alice.read(list_keys[0]).unwrap().as_deref(), Some("101"));
        assert_eq!(bob.read(list_keys[0]).unwrap().as_deref(), Some("10"));
        bob.write("library", "[2]").unwrap();
        assert_ne!(alice, bob);
        assert_eq!(alice.read("library").unwrap().as_deref(), Some("[1]"));
        assert_eq!(bob.read("library").unwrap().as_deref(), Some("[2]"));
        let worker_owner = alice.clone();
        std::thread::spawn(move || worker_owner.write("tracking:durable", "saved after switch"))
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(
            alice.read("tracking:durable").unwrap().as_deref(),
            Some("saved after switch")
        );
        alice.write_cached("home:showcase:v1", "[]").unwrap();
        assert_eq!(
            alice.scan("tracking:").unwrap(),
            vec![("tracking:durable".into(), "saved after switch".into())]
        );
        assert_eq!(bob.read("tracking:durable").unwrap(), None);
        alice
            .persist_snapshot("tracking:durable", "decomposed for Alice", false)
            .unwrap();
        assert_eq!(
            alice.read("tracking:durable").unwrap().as_deref(),
            Some("decomposed for Alice")
        );
        assert_eq!(bob.read("tracking:durable").unwrap(), None);
        legacy.write("library", "legacy raw").unwrap();
        assert_eq!(
            nova_storage::try_get_str("library").unwrap().as_deref(),
            Some("legacy raw")
        );
        let owner = nova_sync::local_store().unwrap();
        {
            let mut store = owner.lock().unwrap();
            store.set(
                super::super::DELETIONS_DOMAIN,
                alice.id(),
                Some("1".into()),
                0,
                9,
            );
            store.save().unwrap();
            // This adapter can be called while a core hook owns the mutex.
            // A durable marker check must never try to take it recursively.
            assert!(alice.write("tracking:durable", "late").is_err());
        }
        assert!(alice.retired().unwrap());
        assert!(alice.write_cached("home:showcase:v1", "late").is_err());
        assert!(
            alice
                .write_batch(&[
                    ("library".into(), Some("late".into())),
                    ("downloads:v1".into(), Some("must stay absent".into()))
                ])
                .is_err()
        );
        assert_eq!(nova_storage::try_get_str("downloads:v1").unwrap(), None);
        alice.write("downloads:v1", "device data").unwrap();
        assert_eq!(
            bob.read("downloads:v1").unwrap().as_deref(),
            Some("device data")
        );
        assert_eq!(alice.key("sync:settings"), "sync:settings");
        assert_eq!(ProfileStore::for_profile(&bob_id), bob);
    }
}

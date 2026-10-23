//! Physical write admission for unattended transfers. Every filesystem write
//! uses the session's wrapper, including already-managed/shared bundles. Manual
//! torrents have no guard and keep their established behavior.
use librqbit::{
    ManagedTorrentShared, TorrentMetadata,
    storage::{
        BoxStorageFactory, StorageFactory, StorageFactoryExt, TorrentStorage,
        filesystem::{FilesystemStorage, FilesystemStorageFactory},
    },
};
use std::{
    collections::HashMap,
    io::IoSlice,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
};

type Check = Arc<dyn Fn(u64) -> bool + Send + Sync>;
#[derive(Clone)]
struct Guard {
    id: u64,
    root: PathBuf,
    check: Check,
}
static GUARDS: LazyLock<Mutex<HashMap<String, Vec<Guard>>>> = LazyLock::new(Mutex::default);
static SERIAL: Mutex<()> = Mutex::new(());
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
pub struct WriteGuard {
    hash: String,
    id: u64,
}
impl Drop for WriteGuard {
    fn drop(&mut self) {
        let mut guards = GUARDS.lock().unwrap();
        if let Some(entries) = guards.get_mut(&self.hash) {
            entries.retain(|guard| guard.id != self.id);
            if entries.is_empty() {
                guards.remove(&self.hash);
            }
        }
    }
}
pub fn register(hash: &str, root: PathBuf, check: Check) -> WriteGuard {
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let hash = hash.to_ascii_lowercase();
    GUARDS
        .lock()
        .unwrap()
        .entry(hash.clone())
        .or_default()
        .push(Guard { id, root, check });
    WriteGuard { hash, id }
}
/// Allocated bytes on Unix include sparse piece-boundary files; elsewhere the
/// logical size is conservative. Symlinks are never followed out of the bundle.
pub fn physical_bytes(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| {
            let Ok(meta) = entry.path().symlink_metadata() else {
                return 0;
            };
            if meta.file_type().is_symlink() {
                return 0;
            }
            if meta.is_dir() {
                return physical_bytes(&entry.path());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                meta.blocks().saturating_mul(512)
            }
            #[cfg(not(unix))]
            {
                meta.len()
            }
        })
        .fold(0, u64::saturating_add)
}
fn admit_metadata(hash: &str, metadata: &TorrentMetadata) -> bool {
    let guards = GUARDS
        .lock()
        .unwrap()
        .get(hash)
        .cloned()
        .unwrap_or_default();
    if guards.is_empty() {
        return true;
    }
    // Opening an entire pack creates empty directory/file entries before piece
    // writes begin. Bound that work too; arbitrary pack trees cannot exhaust
    // metadata space before the first guarded pwrite.
    let entries = metadata
        .file_infos
        .iter()
        .map(|file| file.relative_filename.components().count())
        .sum::<usize>();
    guards.iter().all(|guard| {
        if entries > 1024 {
            (guard.check)(u64::MAX)
        } else {
            (guard.check)(physical_bytes(&guard.root).saturating_add(16 * 1024 * 1024))
        }
    })
}
fn admit(hash: &str, bytes: usize) -> bool {
    let guards = GUARDS
        .lock()
        .unwrap()
        .get(hash)
        .cloned()
        .unwrap_or_default();
    guards.iter().all(|guard| {
        // Block rounding plus two metadata blocks bounds allocation even when
        // a chunk crosses a filesystem block boundary. The same serial lock
        // spans admission + write, so peers cannot each spend the last block.
        let incoming = (bytes as u64).saturating_add(4095) / 4096 * 4096 + 8192;
        (guard.check)(physical_bytes(&guard.root).saturating_add(incoming))
    })
}
#[derive(Clone, Default)]
pub(super) struct GuardedFactory;
impl StorageFactory for GuardedFactory {
    type Storage = GuardedStorage;
    fn create(
        &self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<Self::Storage> {
        Ok(GuardedStorage {
            inner: FilesystemStorageFactory::default().create(shared, metadata)?,
            hash: shared.info_hash.as_string(),
        })
    }
    fn clone_box(&self) -> BoxStorageFactory {
        self.clone().boxed()
    }
}
pub(super) struct GuardedStorage {
    inner: FilesystemStorage,
    hash: String,
}
impl TorrentStorage for GuardedStorage {
    fn init(
        &mut self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<()> {
        let _serial = SERIAL.lock().unwrap();
        if !admit_metadata(&self.hash, metadata) {
            anyhow::bail!("automatic torrent metadata paused by device policy");
        }
        self.inner.init(shared, metadata)
    }
    fn pread_exact(&self, id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.inner.pread_exact(id, offset, buf)
    }
    fn pwrite_all(&self, id: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        let _serial = SERIAL.lock().unwrap();
        if !admit(&self.hash, buf.len()) {
            anyhow::bail!("automatic torrent paused by device policy");
        }
        self.inner.pwrite_all(id, offset, buf)
    }
    fn pwrite_all_vectored(
        &self,
        id: usize,
        offset: u64,
        bufs: [IoSlice<'_>; 2],
    ) -> anyhow::Result<usize> {
        let _serial = SERIAL.lock().unwrap();
        if !admit(&self.hash, bufs.iter().map(|buf| buf.len()).sum()) {
            anyhow::bail!("automatic torrent paused by device policy");
        }
        self.inner.pwrite_all_vectored(id, offset, bufs)
    }
    fn remove_file(&self, id: usize, path: &Path) -> anyhow::Result<()> {
        self.inner.remove_file(id, path)
    }
    fn remove_directory_if_empty(&self, path: &Path) -> anyhow::Result<()> {
        self.inner.remove_directory_if_empty(path)
    }
    fn ensure_file_length(&self, id: usize, len: u64) -> anyhow::Result<()> {
        self.inner.ensure_file_length(id, len)
    }
    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        Ok(Box::new(TakenStorage {
            inner: self.inner.take()?,
            hash: self.hash.clone(),
        }))
    }
}
struct TakenStorage {
    inner: Box<dyn TorrentStorage>,
    hash: String,
}
impl TorrentStorage for TakenStorage {
    fn init(
        &mut self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<()> {
        let _serial = SERIAL.lock().unwrap();
        if !admit_metadata(&self.hash, metadata) {
            anyhow::bail!("automatic torrent metadata paused by device policy");
        }
        self.inner.init(shared, metadata)
    }
    fn pread_exact(&self, id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.inner.pread_exact(id, offset, buf)
    }
    fn pwrite_all(&self, id: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        let _serial = SERIAL.lock().unwrap();
        if !admit(&self.hash, buf.len()) {
            anyhow::bail!("automatic torrent paused by device policy");
        }
        self.inner.pwrite_all(id, offset, buf)
    }
    fn remove_file(&self, id: usize, path: &Path) -> anyhow::Result<()> {
        self.inner.remove_file(id, path)
    }
    fn remove_directory_if_empty(&self, path: &Path) -> anyhow::Result<()> {
        self.inner.remove_directory_if_empty(path)
    }
    fn ensure_file_length(&self, id: usize, len: u64) -> anyhow::Result<()> {
        self.inner.ensure_file_length(id, len)
    }
    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        Ok(Box::new(Self {
            inner: self.inner.take()?,
            hash: self.hash.clone(),
        }))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct FixtureStorage(PathBuf);
    impl TorrentStorage for FixtureStorage {
        fn init(&mut self, _: &ManagedTorrentShared, _: &TorrentMetadata) -> anyhow::Result<()> {
            Ok(())
        }
        fn pread_exact(&self, _: usize, _: u64, _: &mut [u8]) -> anyhow::Result<()> {
            Ok(())
        }
        fn pwrite_all(&self, _: usize, offset: u64, bytes: &[u8]) -> anyhow::Result<()> {
            use std::io::{Seek, Write};
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(&self.0)?;
            file.seek(std::io::SeekFrom::Start(offset))?;
            file.write_all(bytes)?;
            Ok(())
        }
        fn remove_file(&self, _: usize, _: &Path) -> anyhow::Result<()> {
            Ok(())
        }
        fn remove_directory_if_empty(&self, _: &Path) -> anyhow::Result<()> {
            Ok(())
        }
        fn ensure_file_length(&self, _: usize, _: u64) -> anyhow::Result<()> {
            Ok(())
        }
        fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
            Ok(Box::new(Self(self.0.clone())))
        }
    }
    #[test]
    fn concurrent_peer_writes_cannot_spend_the_same_last_block() {
        let root = std::env::temp_dir().join(format!("nova-quota-race-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let guard = register("quota-race", root.clone(), Arc::new(|next| next <= 12288));
        let storage = Arc::new(TakenStorage {
            inner: Box::new(FixtureStorage(root.join("file"))),
            hash: "quota-race".into(),
        });
        let first = storage.clone();
        let a = std::thread::spawn(move || first.pwrite_all(0, 0, b"x"));
        let second = storage.clone();
        let b = std::thread::spawn(move || second.pwrite_all(0, 4096, b"y"));
        let results = [a.join().unwrap(), b.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(physical_bytes(&root), 4096);
        drop(guard);
        storage.pwrite_all(0, 8192, b"z").unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shared_bundle_guard_is_dynamic_and_manual_paths_remain_unrestricted() {
        let root = std::env::temp_dir().join(format!("nova-write-guard-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let guard = register("abc", root.clone(), Arc::new(|next| next <= 16384));
        assert!(admit("abc", 1));
        assert!(!admit("abc", 16384));
        assert!(admit("manual", usize::MAX / 2));
        let sibling = register("abc", root.clone(), Arc::new(|_| false));
        assert!(!admit("abc", 1));
        drop(sibling);
        assert!(admit("abc", 1));
        drop(guard);
        assert!(admit("abc", usize::MAX / 2));
        let _ = std::fs::remove_dir_all(root);
    }
}

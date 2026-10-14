//! Disposable provider caches belong to the OS cache directory, not app data.
use nova_providers::{ProviderHostError, plugin_settings as service};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex, Once, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

const LEGACY_KEY: &str = "plugin_cache:v1";
// The service permits 20 MiB of logical values. JSON escaping can expand those
// bytes sixfold; reject larger files before allocating an unbounded string.
const MAX_FILE_BYTES: u64 = 121 * 1024 * 1024;
const GC_INTERVAL: Duration = Duration::from_secs(120);
static GC_THREAD: OnceLock<std::thread::Thread> = OnceLock::new();
static FILE_MUTATION: Mutex<()> = Mutex::new(());
const TEMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

fn temporary_name(name: &str) -> bool {
    let Some((pid, sequence)) = name
        .strip_prefix(".cache-")
        .and_then(|name| name.strip_suffix(".tmp"))
        .and_then(|name| name.split_once('-'))
    else {
        return false;
    };
    // Only names emitted by write_file belong to this collector.
    pid.parse::<u32>()
        .is_ok_and(|value| value.to_string() == pid)
        && sequence
            .parse::<u64>()
            .is_ok_and(|value| value.to_string() == sequence)
}

fn prune_temporary_files(directory: &Path, now: SystemTime) -> std::io::Result<()> {
    let _mutation = FILE_MUTATION.lock().unwrap();
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    // A crash can bypass write_file's cleanup. Leave fresh writes alone and
    // bound work if an unrelated process has populated the cache directory.
    for entry in entries.take(4096) {
        let entry = entry?;
        if !entry.file_name().to_str().is_some_and(temporary_name) {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if metadata.is_file()
            && metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age >= TEMP_MAX_AGE)
        {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn path() -> PathBuf {
    nova_config::app_cache_dir().join("plugins/cache-v1.json")
}

fn read_file(path: &Path) -> std::io::Result<Option<String>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > MAX_FILE_BYTES {
        return Err(std::io::Error::other("plugin cache exceeds file limit"));
    }
    let mut raw = String::new();
    file.take(MAX_FILE_BYTES + 1).read_to_string(&mut raw)?;
    if raw.len() as u64 > MAX_FILE_BYTES
        || !serde_json::from_str::<serde_json::Value>(&raw).is_ok_and(|value| value.is_object())
    {
        return Err(std::io::Error::other("invalid plugin cache file"));
    }
    Ok(Some(raw))
}

fn write_file(path: &Path, raw: &str) -> std::io::Result<()> {
    let _mutation = FILE_MUTATION.lock().unwrap();
    if raw.len() as u64 > MAX_FILE_BYTES {
        return Err(std::io::Error::other("plugin cache exceeds file limit"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing cache directory"))?;
    fs::create_dir_all(parent)?;
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let temporary = parent.join(format!(
        ".cache-{}-{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(raw.as_bytes())?;
        // Cache data is reconstructible; atomic replacement suffices without
        // forcing each search to wait for a durable filesystem sync.
        drop(file);
        fs::rename(&temporary, path)
    })();
    let _ = fs::remove_file(temporary);
    result
}

fn remove_legacy() {
    if let Err(error) = crate::storage::try_write_batch(&[(LEGACY_KEY.into(), None)]) {
        tracing::warn!(%error, "could not remove legacy provider cache");
    }
}

pub(super) fn load() -> Result<Option<String>, ProviderHostError> {
    let path = path();
    match read_file(&path) {
        Ok(Some(raw)) => {
            remove_legacy();
            Ok(Some(raw))
        }
        Ok(None) => {
            // Move the previous discovery cache once. Removing its app-data
            // copy prevents Android Clear cache from restoring stale entries.
            match crate::storage::try_get_str(LEGACY_KEY) {
                Ok(Some(raw)) => {
                    if let Err(error) = write_file(&path, &raw) {
                        tracing::warn!(%error, "could not migrate provider cache");
                    }
                    remove_legacy();
                    Ok(Some(raw))
                }
                Ok(None) => Ok(None),
                Err(error) => {
                    tracing::warn!(%error, "discarding unreadable legacy provider cache");
                    remove_legacy();
                    Ok(None)
                }
            }
        }
        Err(error) => {
            // A corrupt or evicted cache must never block settings or startup.
            tracing::warn!(%error, "discarding unreadable provider cache");
            let _ = fs::remove_file(path);
            remove_legacy();
            Ok(None)
        }
    }
}

pub(super) fn save(raw: &str) -> Result<(), ProviderHostError> {
    write_file(&path(), raw).map_err(|error| ProviderHostError(error.to_string()))
}

pub(super) fn wake_gc_worker() {
    if let Some(worker) = GC_THREAD.get() {
        worker.unpark();
    }
}

pub(super) fn start_gc_worker() {
    static START: Once = Once::new();
    START.call_once(|| {
        if let Err(error) = std::thread::Builder::new()
            .name("nova-plugin-cache-gc".into())
            .spawn(|| {
                let _ = GC_THREAD.set(std::thread::current());
                let directory = path().parent().unwrap().to_owned();
                let mut previous = None;
                let mut marker_loaded = false;
                loop {
                    let result = (|| -> std::io::Result<()> {
                        if !marker_loaded {
                            previous =
                                super::cache_gc::last_cleanup("cache_gc:providers:v1", &directory)?;
                            marker_loaded = true;
                        }
                        let now = nova_config::now_secs();
                        if nova_media::cache::gc::cleanup_due(previous, now) {
                            prune_temporary_files(&directory, SystemTime::now())?;
                            let removed = service::cache_gc(now).map_err(std::io::Error::other)?;
                            super::cache_gc::save_cleanup(
                                "cache_gc:providers:v1",
                                &directory,
                                now,
                            )?;
                            previous = Some(now);
                            if removed > 0 {
                                tracing::info!(
                                    entries = removed,
                                    "provider cache cleanup completed"
                                );
                            }
                        }
                        Ok(())
                    })();
                    if let Err(error) = result {
                        tracing::warn!(%error, "provider cache cleanup failed");
                    }
                    // This wake only compares an in-memory timestamp. Files
                    // and the service cache are examined once a day, not every wake.
                    std::thread::park_timeout(GC_INTERVAL);
                }
            })
        {
            tracing::warn!(%error, "could not start provider cache cleanup");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_removes_legacy_cache_without_resurrection_after_eviction() {
        // Storage and cache-directory configuration are process-global. Keep
        // this production migration path independent of other app fixtures.
        const TEST_DIR: &str = "NOVA_PLUGIN_CACHE_MIGRATION_TEST_DIR";
        let Some(directory) = std::env::var_os(TEST_DIR) else {
            let directory = std::env::temp_dir().join(format!(
                "nova-plugin-cache-migration-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(TEST_DIR, &directory)
                .env("XDG_CACHE_HOME", directory.join("cache"))
                .env("LOCALAPPDATA", directory.join("cache"))
                .env("NOVA_CACHE_DIR", directory.join("cache"))
                .args([
                    "--exact",
                    "app::plugin_cache::tests::migration_removes_legacy_cache_without_resurrection_after_eviction",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            let _ = fs::remove_dir_all(directory);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        crate::storage::init_at(&PathBuf::from(directory).join("storage"));
        let marker_key = "cache_gc:providers:v1";
        let marker_directory = path().parent().unwrap().to_owned();
        assert_eq!(
            super::super::cache_gc::last_cleanup(marker_key, &marker_directory).unwrap(),
            None
        );
        super::super::cache_gc::save_cleanup(marker_key, &marker_directory, 1234).unwrap();
        assert_eq!(
            super::super::cache_gc::last_cleanup(marker_key, &marker_directory).unwrap(),
            Some(1234)
        );
        assert_eq!(
            super::super::cache_gc::last_cleanup(marker_key, &marker_directory.join("moved"))
                .unwrap(),
            None
        );
        crate::storage::try_set_str(marker_key, "broken").unwrap();
        assert_eq!(
            super::super::cache_gc::last_cleanup(marker_key, &marker_directory).unwrap(),
            None
        );
        let legacy = r#"{"example/key":{"plugin":"example","key":"key","value":"cached","expires":200,"used":1}}"#;
        crate::storage::try_set_cached_str(LEGACY_KEY, legacy).unwrap();
        assert_eq!(load().unwrap().as_deref(), Some(legacy));
        assert_eq!(read_file(&path()).unwrap().as_deref(), Some(legacy));
        assert!(crate::storage::try_get_str(LEGACY_KEY).unwrap().is_none());
        fs::remove_file(path()).unwrap();
        assert!(load().unwrap().is_none());
        assert!(!path().exists());

        // A bad cache file is disposable and must not revive an old app-data
        // cache or prevent preference loading through Backend::load_cache.
        crate::storage::try_set_cached_str(LEGACY_KEY, legacy).unwrap();
        fs::write(path(), "broken").unwrap();
        assert!(load().unwrap().is_none());
        assert!(crate::storage::try_get_str(LEGACY_KEY).unwrap().is_none());
        assert!(!path().exists());
    }

    #[test]
    fn temporary_gc_removes_only_old_generated_regular_files() {
        let directory =
            std::env::temp_dir().join(format!("nova-plugin-cache-temp-gc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let now = SystemTime::now();
        for (name, age) in [
            (".cache-12-0.tmp", TEMP_MAX_AGE),
            (".cache-12-1.tmp", Duration::from_secs(60)),
            (".cache-12-extra.tmp", TEMP_MAX_AGE),
            (".cache-012-2.tmp", TEMP_MAX_AGE),
            ("cache-v1.json", TEMP_MAX_AGE),
        ] {
            let file = fs::File::create(directory.join(name)).unwrap();
            file.set_times(fs::FileTimes::new().set_modified(now - age))
                .unwrap();
        }
        fs::create_dir(directory.join(".cache-12-3.tmp")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            directory.join("cache-v1.json"),
            directory.join(".cache-12-4.tmp"),
        )
        .unwrap();
        prune_temporary_files(&directory, now).unwrap();
        assert!(!directory.join(".cache-12-0.tmp").exists());
        for name in [
            ".cache-12-1.tmp",
            ".cache-12-extra.tmp",
            ".cache-012-2.tmp",
            ".cache-12-3.tmp",
            "cache-v1.json",
        ] {
            assert!(directory.join(name).exists(), "removed {name}");
        }
        #[cfg(unix)]
        assert!(
            fs::symlink_metadata(directory.join(".cache-12-4.tmp"))
                .unwrap()
                .is_symlink()
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn disk_cache_reopens_replaces_and_tolerates_system_eviction() {
        let dir = std::env::temp_dir().join(format!("nova-plugin-cache-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("plugins/cache-v1.json");
        assert!(read_file(&path).unwrap().is_none());
        write_file(&path, r#"{"example":"first"}"#).unwrap();
        assert_eq!(read_file(&path).unwrap().unwrap(), r#"{"example":"first"}"#);
        write_file(&path, "{}").unwrap();
        assert_eq!(read_file(&path).unwrap().unwrap(), "{}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::write(&path, "broken").unwrap();
        assert!(read_file(&path).is_err());
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(read_file(&path).is_err());
        fs::remove_dir_all(&dir).unwrap();
        assert!(read_file(&path).unwrap().is_none());
        write_file(&path, "{}").unwrap();
        assert_eq!(read_file(&path).unwrap().unwrap(), "{}");
        fs::remove_dir_all(dir).unwrap();
    }
}

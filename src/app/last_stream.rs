//! One expiring stream choice shared through sync; never used to start playback.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
    time::Duration,
};

pub(super) const DOMAIN: &str = "last_stream";
const SELECTION: &str = "selection";

const TTL: u64 = 7 * 24 * 60 * 60;
const MAX_BYTES: u64 = 512;
const SERIES_MAX_BYTES: u64 = 128 * 1024;
const SERIES_TTL: u64 = 90 * 24 * 60 * 60;
const SERIES_LIMIT: usize = 128;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct SeriesChoice {
    preference: super::streams::StreamPreference,
    selected_at: u64,
}

type SeriesChoices = std::collections::BTreeMap<String, SeriesChoice>;

fn prune_series(choices: &mut SeriesChoices, now: u64) {
    choices.retain(|_, choice| now >= choice.selected_at && now - choice.selected_at < SERIES_TTL);
    while choices.len() > SERIES_LIMIT {
        let oldest = choices
            .iter()
            .min_by_key(|(_, choice)| choice.selected_at)
            .map(|(key, _)| key.clone())
            .unwrap();
        choices.remove(&oldest);
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Record {
    version: u8,
    identity: String,
    selected_at: u64,
}

impl Record {
    fn valid(&self, now: u64) -> bool {
        self.version == 1
            && self
                .identity
                .strip_prefix("stream-")
                .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()))
            && now >= self.selected_at
            && now - self.selected_at < TTL
    }
}

#[derive(Default)]
pub(super) struct Cache {
    record: Arc<Mutex<Option<Record>>>,
    series: Arc<Mutex<SeriesChoices>>,
    wake: Option<mpsc::Sender<()>>,
    worker: Option<JoinHandle<()>>,
    synced: bool,
}

impl Cache {
    /// The ordinary constructor stays memory-only for bridge fixtures. Only
    /// the production entry point opens the app-private cache file.
    #[cfg(test)]
    pub(super) fn open(path: PathBuf) -> Self {
        Self::open_inner(path, false)
    }

    pub(super) fn open_synced(path: PathBuf) -> Self {
        Self::open_inner(path, true)
    }

    fn open_inner(path: PathBuf, synced: bool) -> Self {
        let now = nova_config::now_secs();
        let record = read(&path).filter(|record| record.valid(now));
        let record = Arc::new(Mutex::new(record));
        let state = record.clone();
        let series_path = path.with_file_name("series-stream-v1.json");
        let mut series_choices: SeriesChoices = fs::File::open(&series_path)
            .ok()
            .and_then(|file| {
                let mut raw = Vec::new();
                file.take(SERIES_MAX_BYTES + 1).read_to_end(&mut raw).ok()?;
                (raw.len() as u64 <= SERIES_MAX_BYTES)
                    .then(|| serde_json::from_slice(&raw).ok())
                    .flatten()
            })
            .unwrap_or_default();
        prune_series(&mut series_choices, now);
        let series = Arc::new(Mutex::new(series_choices));
        let series_state = series.clone();
        let (wake, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("nova-last-stream".into())
            .spawn(move || persist_loop(path, state, series_path, series_state, receiver, synced))
        {
            Ok(worker) => Self {
                record,
                series,
                wake: Some(wake),
                worker: Some(worker),
                synced,
            },
            Err(error) => {
                tracing::warn!(%error, "could not start stream choice cache");
                Self {
                    record,
                    series,
                    wake: None,
                    worker: None,
                    synced,
                }
            }
        }
    }

    pub(super) fn remember_series(
        &self,
        series_id: &str,
        preference: super::streams::StreamPreference,
        now: u64,
    ) {
        let mut series = self.series.lock().unwrap();
        series.insert(
            super::streams::preference_hash("series", series_id),
            SeriesChoice {
                preference,
                selected_at: now,
            },
        );
        prune_series(&mut series, now);
        drop(series);
        self.notify();
    }

    pub(super) fn series_preference(
        &self,
        series_id: &str,
        now: u64,
    ) -> Option<super::streams::StreamPreference> {
        let mut series = self.series.lock().unwrap();
        prune_series(&mut series, now);
        series
            .get(&super::streams::preference_hash("series", series_id))
            .map(|choice| choice.preference.clone())
    }

    pub(super) fn remember(&self, identity: String, now: u64) {
        let record = Record {
            version: 1,
            identity,
            selected_at: now,
        };
        if !record.valid(now) {
            return;
        }
        if self.synced && !super::sync::applying() {
            let result = (|| -> nova_sync::Result<()> {
                let owner = nova_sync::local_store()?;
                let dev = nova_sync::local_device()?;
                let mut store = owner.lock().unwrap();
                let changed = store.set(
                    DOMAIN,
                    SELECTION,
                    Some(serde_json::to_string(&record)?),
                    now,
                    dev,
                );
                store.save()?;
                drop(store);
                if changed {
                    nova_sync::request_local_sync();
                }
                Ok(())
            })();
            if let Err(error) = result {
                super::io::record_write_failure(nova_storage::Error::new(
                    nova_storage::ErrorKind::Transaction,
                    error,
                ));
            }
        }
        *self.record.lock().unwrap() = Some(record);
        self.notify();
    }

    fn project(&self, raw: Option<&str>, now: u64) -> Result<(), String> {
        let incoming = decode(raw, now)?;
        *self.record.lock().unwrap() = incoming;
        // Projection only updates the local file. It must not publish a new
        // selection or renew the original selection's seven-day lifetime.
        self.notify();
        Ok(())
    }

    pub(super) fn identity(&self, now: u64) -> Option<String> {
        let mut record = self.record.lock().unwrap();
        if record.as_ref().is_some_and(|record| !record.valid(now)) {
            *record = None;
            self.notify();
        }
        record.as_ref().map(|record| record.identity.clone())
    }

    fn notify(&self) {
        if let Some(wake) = &self.wake {
            let _ = wake.send(());
        }
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        self.wake.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn read(path: &Path) -> Option<Record> {
    let file = fs::File::open(path).ok()?;
    let mut raw = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut raw).ok()?;
    (raw.len() as u64 <= MAX_BYTES)
        .then(|| serde_json::from_slice(&raw).ok())
        .flatten()
}

fn persist<T: Serialize>(path: &Path, record: Option<&T>) -> io::Result<()> {
    let Some(record) = record else {
        return match fs::remove_file(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        };
    };
    let raw = serde_json::to_vec(record)?;
    let parent = path.parent().expect("cache file has a parent");
    fs::create_dir_all(parent)?;
    // No playback URL, token or display text is written. Atomic replacement
    // suffices for reconstructible cache data; don't fsync during playback.
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&raw)?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    let _ = fs::remove_file(temporary);
    result
}

fn snapshot(state: &Mutex<Option<Record>>) -> Option<Record> {
    let mut state = state.lock().unwrap();
    if state
        .as_ref()
        .is_some_and(|record| !record.valid(nova_config::now_secs()))
    {
        *state = None;
    }
    state.clone()
}

fn persist_loop(
    path: PathBuf,
    state: Arc<Mutex<Option<Record>>>,
    series_path: PathBuf,
    series: Arc<Mutex<SeriesChoices>>,
    receiver: mpsc::Receiver<()>,
    synced: bool,
) {
    let mut saved = None;
    let mut saved_series = None;
    let mut first = true;
    loop {
        // Coalesce pending selections. Cache filesystem work happens here,
        // outside the UI thread and outside the in-memory record lock.
        for _ in receiver.try_iter() {}
        if synced && let Err(error) = expire_synced_choice(nova_config::now_secs()) {
            super::io::record_write_failure(nova_storage::Error::new(
                nova_storage::ErrorKind::Transaction,
                error,
            ));
        }
        let current_series = {
            let mut choices = series.lock().unwrap();
            prune_series(&mut choices, nova_config::now_secs());
            choices.clone()
        };
        if saved_series.as_ref() != Some(&current_series) {
            match persist(&series_path, Some(&current_series)) {
                Ok(()) => saved_series = Some(current_series),
                Err(error) => tracing::warn!(%error, "could not persist series stream preferences"),
            }
        }
        let current = snapshot(&state);
        if first || current != saved {
            match persist(&path, current.as_ref()) {
                Ok(()) => {
                    saved = current.clone();
                    first = false;
                }
                Err(error) => tracing::warn!(%error, "could not persist stream choice cache"),
            }
        }
        let wait = current.as_ref().map_or(3600, |record| {
            record
                .selected_at
                .saturating_add(TTL)
                .saturating_sub(nova_config::now_secs())
                .clamp(1, 3600)
        });
        if matches!(
            receiver.recv_timeout(Duration::from_secs(wait)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ) {
            // A final selection may have raced the last snapshot. Flush it
            // before an orderly shutdown; process crashes simply lose a hint.
            if let Err(error) = persist(&path, snapshot(&state).as_ref()) {
                tracing::warn!(%error, "could not flush stream choice cache");
            }
            if let Err(error) = persist(&series_path, Some(&*series.lock().unwrap())) {
                tracing::warn!(%error, "could not flush series stream preferences");
            }
            return;
        }
    }
}

/// Malformed payloads remain pending for inspection; expired and future-clock
/// hints simply do not promote a stream. Peer data never becomes a file path.
fn decode(raw: Option<&str>, now: u64) -> Result<Option<Record>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.len() as u64 > MAX_BYTES {
        return Err("stream choice exceeds size limit".into());
    }
    let record: Record = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    if record.version != 1
        || !record
            .identity
            .strip_prefix("stream-")
            .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        return Err("invalid stream choice identity or version".into());
    }
    Ok(record.valid(now).then_some(record))
}

fn expire_in_store(store: &mut nova_sync::Store, now: u64) -> bool {
    let expired = store
        .record(DOMAIN, SELECTION)
        .and_then(|record| record.value.as_deref())
        .filter(|raw| raw.len() as u64 <= MAX_BYTES)
        .and_then(|raw| serde_json::from_str::<Record>(raw).ok())
        .is_some_and(|record| record.version == 1 && now.saturating_sub(record.selected_at) >= TTL);
    // Check the authoritative record under its lock, not the expired cache
    // snapshot: a late cleanup must never delete another device's newer choice.
    // Expiry is not a new user action. Keep the selection's original version
    // and break its exact tie with a tombstone; a newer offline selection must
    // still win when received after this cleanup.
    if !expired {
        return false;
    }
    let mut tombstone = store.record(DOMAIN, SELECTION).unwrap().clone();
    tombstone.value = None;
    tombstone.version.deleted = true;
    store.apply(DOMAIN, SELECTION, tombstone)
}

fn expire_synced_choice(now: u64) -> nova_sync::Result<()> {
    let owner = nova_sync::local_store()?;
    let mut store = owner.lock().unwrap();
    let changed = expire_in_store(&mut store, now);
    if changed {
        store.save()?;
    }
    drop(store);
    if changed {
        nova_sync::request_local_sync();
    }
    Ok(())
}

fn seed_legacy_choice(
    store: &mut nova_sync::Store,
    dev: u64,
    record: Option<Record>,
) -> nova_sync::Result<bool> {
    if store.record(DOMAIN, SELECTION).is_some() {
        return Ok(false);
    }
    let Some(record) = record else {
        return Ok(false);
    };
    // Migration is not a fresh selection. Retain its historical clock so an
    // older choice upgraded later cannot beat a recently selected peer choice.
    Ok(store.apply(
        DOMAIN,
        SELECTION,
        nova_sync::Record {
            value: Some(serde_json::to_string(&record)?),
            version: nova_sync::Version::new(
                record.selected_at.saturating_mul(1000),
                0,
                dev,
                false,
            ),
        },
    ))
}

impl super::Bridge {
    /// Seed the legacy cache with its original selection clock, then restore
    /// the record owner on every startup (including domains older apps ignored).
    pub(super) fn restore_last_stream_choice(&self) {
        let result = (|| -> nova_sync::Result<()> {
            let owner = nova_sync::local_store()?;
            let dev = nova_sync::local_device()?;
            let mut store = owner.lock().unwrap();
            if seed_legacy_choice(&mut store, dev, snapshot(&self.last_stream.record))? {
                store.save()?;
            }
            drop(store);
            self.project_last_stream_choice().map_err(|error| {
                nova_storage::Error::new(nova_storage::ErrorKind::Schema, error)
            })?;
            Ok(())
        })();
        if let Err(error) = result {
            super::io::record_write_failure(nova_storage::Error::new(
                nova_storage::ErrorKind::Transaction,
                error,
            ));
        }
    }

    pub(super) fn project_last_stream_choice(&self) -> Result<(), String> {
        let _guard = super::sync::ApplyingGuard::new();
        let owner = nova_sync::local_store().map_err(|e| e.to_string())?;
        let raw = owner
            .lock()
            .unwrap()
            .record(DOMAIN, SELECTION)
            .and_then(|record| record.value.clone());
        self.last_stream
            .project(raw.as_deref(), nova_config::now_secs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(c: char) -> String {
        format!("stream-{}", c.to_string().repeat(64))
    }

    fn choice(c: char, selected_at: u64) -> Record {
        Record {
            version: 1,
            identity: identity(c),
            selected_at,
        }
    }

    #[test]
    fn series_preferences_survive_restart_stay_show_scoped_and_expire() {
        let directory = std::env::temp_dir().join(format!(
            "nova-series-stream-{}-{}",
            std::process::id(),
            nova_config::now_ms()
        ));
        let path = directory.join("last-stream-v1.json");
        let now = nova_config::now_secs();
        let preference = super::super::streams::StreamPreference::default();
        {
            let cache = Cache::open(path.clone());
            cache.remember_series("show-a", preference.clone(), now);
            assert!(cache.series_preference("show-a", now).is_some());
            assert!(cache.series_preference("show-b", now).is_none());
            cache.remember_series("show-b", preference, now);
            assert_eq!(cache.series.lock().unwrap().len(), 2);
        }
        let cache = Cache::open(path);
        assert!(cache.series_preference("show-a", now).is_some());
        assert!(cache.series_preference("show-b", now).is_some());
        assert!(
            cache
                .series_preference("show-a", now + SERIES_TTL)
                .is_none()
        );
        assert!(
            !fs::read_to_string(directory.join("series-stream-v1.json"))
                .unwrap()
                .contains("show-a")
        );
        drop(cache);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn peers_share_one_choice_without_echoing_or_renewing_its_expiry() {
        let now = nova_config::now_secs();
        let mut a = nova_sync::Store::default();
        let mut b = nova_sync::Store::default();
        seed_legacy_choice(&mut a, 1, Some(choice('a', now - 100))).unwrap();
        let old = a.record(DOMAIN, SELECTION).unwrap().clone();
        b.apply(DOMAIN, SELECTION, old.clone());
        let remote = serde_json::to_string(&choice('b', now - 10)).unwrap();
        b.set(DOMAIN, SELECTION, Some(remote.clone()), now - 10, 2);
        a.apply(
            DOMAIN,
            SELECTION,
            b.record(DOMAIN, SELECTION).unwrap().clone(),
        );
        let cache = Cache::default();
        let before = a.digest();
        cache
            .project(a.record(DOMAIN, SELECTION).unwrap().value.as_deref(), now)
            .unwrap();
        assert_eq!(cache.identity(now), Some(identity('b')));
        assert_eq!(a.digest(), before, "projection must not mint a new version");
        assert!(!a.apply(DOMAIN, SELECTION, old));
        assert_eq!(cache.identity(now - 10 + TTL), None);
        assert_eq!(a.records(DOMAIN).len(), 1);

        // A new local selection after receipt sorts after the observed peer.
        a.set(
            DOMAIN,
            SELECTION,
            Some(serde_json::to_string(&choice('c', now)).unwrap()),
            now,
            1,
        );
        b.apply(
            DOMAIN,
            SELECTION,
            a.record(DOMAIN, SELECTION).unwrap().clone(),
        );
        cache
            .project(b.record(DOMAIN, SELECTION).unwrap().value.as_deref(), now)
            .unwrap();
        assert_eq!(cache.identity(now), Some(identity('c')));
        let digest = b.digest();
        assert!(!b.set(
            DOMAIN,
            SELECTION,
            b.record(DOMAIN, SELECTION).unwrap().value.clone(),
            now,
            2
        ));
        assert_eq!(b.digest(), digest);
    }

    #[test]
    fn expiry_checks_the_owner_and_migration_cannot_revive_an_old_choice() {
        let now = nova_config::now_secs();
        let mut store = nova_sync::Store::default();
        seed_legacy_choice(&mut store, 1, Some(choice('a', now - TTL))).unwrap();
        // The file worker still has the old hint, but the owner has a new one.
        store.set(
            DOMAIN,
            SELECTION,
            Some(serde_json::to_string(&choice('b', now)).unwrap()),
            now,
            2,
        );
        assert!(!expire_in_store(&mut store, now));
        assert!(!seed_legacy_choice(&mut store, 1, Some(choice('a', now - 100))).unwrap());
        assert!(expire_in_store(&mut store, now + TTL));
        let tombstone = store.record(DOMAIN, SELECTION).unwrap().clone();
        assert!(tombstone.is_deleted());
        assert!(!seed_legacy_choice(&mut store, 1, Some(choice('a', now))).unwrap());
        assert!(!expire_in_store(&mut store, now + TTL));
        let mut peer = nova_sync::Store::default();
        peer.apply(DOMAIN, SELECTION, tombstone);
        let cache = Cache::default();
        cache.remember(identity('a'), now);
        cache
            .project(
                peer.record(DOMAIN, SELECTION).unwrap().value.as_deref(),
                now,
            )
            .unwrap();
        assert_eq!(cache.identity(now), None);
        // Historical migration clocks converge independently of upgrade order.
        let mut first = nova_sync::Store::default();
        let mut second = nova_sync::Store::default();
        seed_legacy_choice(&mut first, 1, Some(choice('a', now - 100))).unwrap();
        seed_legacy_choice(&mut second, 2, Some(choice('b', now - 10))).unwrap();
        let newer = second.record(DOMAIN, SELECTION).unwrap().clone();
        first.apply(DOMAIN, SELECTION, newer);
        assert!(
            first
                .record(DOMAIN, SELECTION)
                .unwrap()
                .value
                .as_ref()
                .unwrap()
                .contains(&identity('b'))
        );
    }

    #[test]
    fn cleanup_cannot_erase_a_newer_selection_that_was_offline() {
        let now = nova_config::now_secs();
        let mut a = nova_sync::Store::default();
        let mut b = nova_sync::Store::default();
        seed_legacy_choice(&mut a, 1, Some(choice('a', now - TTL))).unwrap();
        let old = a.record(DOMAIN, SELECTION).unwrap().clone();
        b.apply(DOMAIN, SELECTION, old.clone());
        b.set(
            DOMAIN,
            SELECTION,
            Some(serde_json::to_string(&choice('b', now)).unwrap()),
            now,
            2,
        );
        assert!(expire_in_store(&mut a, now));
        let tombstone = a.record(DOMAIN, SELECTION).unwrap().clone();
        assert_eq!(tombstone.version.ts, old.version.ts);
        assert_eq!(tombstone.version.counter, old.version.counter);
        b.apply(DOMAIN, SELECTION, tombstone);
        let newest = b.record(DOMAIN, SELECTION).unwrap().clone();
        assert!(!newest.is_deleted());
        a.apply(DOMAIN, SELECTION, newest.clone());
        assert_eq!(a.record(DOMAIN, SELECTION), Some(&newest));
    }

    #[test]
    fn peer_choices_are_bounded_and_cannot_contain_playback_urls() {
        let now = nova_config::now_secs();
        let cache = Cache::default();
        cache.remember(identity('a'), now);
        let invalid = serde_json::to_string(&Record {
            version: 1,
            identity: "https://example.invalid/token".into(),
            selected_at: now,
        })
        .unwrap();
        assert!(cache.project(Some(&invalid), now).is_err());
        assert!(
            cache
                .project(Some(&"x".repeat(MAX_BYTES as usize + 1)), now)
                .is_err()
        );
        assert_eq!(cache.identity(now), Some(identity('a')));
        cache
            .project(
                Some(&serde_json::to_string(&choice('b', now - TTL)).unwrap()),
                now,
            )
            .unwrap();
        assert_eq!(cache.identity(now), None);
        cache
            .project(
                Some(&serde_json::to_string(&choice('b', now + 1)).unwrap()),
                now,
            )
            .unwrap();
        assert_eq!(cache.identity(now), None);
    }

    #[test]
    fn one_choice_replaces_the_previous_without_extending_expiry_on_reads() {
        let cache = Cache::default();
        cache.remember(identity('a'), 100);
        cache.remember(identity('b'), 200);
        assert_eq!(cache.identity(100 + TTL), Some(identity('b')));
        assert_eq!(cache.identity(199 + TTL), Some(identity('b')));
        assert_eq!(cache.identity(200 + TTL), None);
        cache.remember(identity('c'), 200);
        assert_eq!(
            cache.identity(199),
            None,
            "future clocks invalidate the hint"
        );
        cache.remember("https://example.invalid/private-token".into(), 200);
        assert_eq!(cache.identity(200), None);
    }

    #[test]
    fn choice_survives_restart_and_expired_or_corrupt_disk_records_are_deleted() {
        let directory = std::env::temp_dir().join(format!(
            "nova-last-stream-{}-{}",
            std::process::id(),
            nova_config::now_ms()
        ));
        let path = directory.join("last-stream-v1.json");
        let now = nova_config::now_secs();
        {
            let cache = Cache::open(path.clone());
            cache.remember(identity('a'), now);
            cache.remember(identity('b'), now);
        }
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.len() < MAX_BYTES as usize);
        assert!(!raw.contains(&identity('a')));
        assert_eq!(Cache::open(path.clone()).identity(now), Some(identity('b')));
        persist(
            &path,
            Some(&Record {
                version: 1,
                identity: identity('c'),
                selected_at: now - TTL,
            }),
        )
        .unwrap();
        assert_eq!(Cache::open(path.clone()).identity(now), None);
        assert!(!path.exists());
        fs::write(&path, [b'x'; MAX_BYTES as usize + 1]).unwrap();
        assert_eq!(Cache::open(path.clone()).identity(now), None);
        assert!(!path.exists());
        fs::remove_dir_all(directory).unwrap();
    }
}

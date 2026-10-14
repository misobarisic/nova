//! One disposable, device-local stream choice; never synced or used to play.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
    time::Duration,
};

const TTL: u64 = 7 * 24 * 60 * 60;
const MAX_BYTES: u64 = 512;

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
    wake: Option<mpsc::Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl Cache {
    /// The ordinary constructor stays memory-only for bridge fixtures. Only
    /// the production entry point opens the app-private cache file.
    pub(super) fn open(path: PathBuf) -> Self {
        let now = nova_config::now_secs();
        let record = read(&path).filter(|record| record.valid(now));
        let record = Arc::new(Mutex::new(record));
        let state = record.clone();
        let (wake, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("nova-last-stream".into())
            .spawn(move || persist_loop(path, state, receiver))
        {
            Ok(worker) => Self {
                record,
                wake: Some(wake),
                worker: Some(worker),
            },
            Err(error) => {
                tracing::warn!(%error, "could not start stream choice cache");
                Self {
                    record,
                    wake: None,
                    worker: None,
                }
            }
        }
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
        *self.record.lock().unwrap() = Some(record);
        self.notify();
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

fn persist(path: &Path, record: Option<&Record>) -> io::Result<()> {
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

fn persist_loop(path: PathBuf, state: Arc<Mutex<Option<Record>>>, receiver: mpsc::Receiver<()>) {
    let mut saved = None;
    let mut first = true;
    loop {
        // Coalesce pending selections. All filesystem work happens here,
        // outside the UI thread and outside the in-memory record lock.
        for _ in receiver.try_iter() {}
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
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(c: char) -> String {
        format!("stream-{}", c.to_string().repeat(64))
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

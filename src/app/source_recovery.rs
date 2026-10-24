//! Device-local playback evidence and conservative recovery policy.
//! No resolved links, credentials or inferred hardware capability are retained.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const TTL: u64 = 30 * 24 * 60 * 60;
const LIMIT: usize = 128;
const MAX_BYTES: u64 = 24 * 1024;

#[derive(Default, Serialize, Deserialize)]
struct Successes(BTreeMap<String, u64>);
impl Successes {
    fn prune(&mut self, now: u64) {
        self.0
            .retain(|key, at| valid_fingerprint(key) && now >= *at && now - *at < TTL);
        while self.0.len() > LIMIT {
            let oldest = self
                .0
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(key, _)| key.clone())
                .unwrap();
            self.0.remove(&oldest);
        }
    }
}
fn valid_fingerprint(key: &str) -> bool {
    key.strip_prefix("stream-")
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()))
}
fn cache() -> &'static Mutex<Successes> {
    static CACHE: OnceLock<Mutex<Successes>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let path = nova_config::app_cache_dir().join("source-success-v1.json");
        let mut raw = String::new();
        let mut values: Successes = std::fs::File::open(path)
            .ok()
            .and_then(|file| {
                file.take(MAX_BYTES + 1).read_to_string(&mut raw).ok()?;
                (raw.len() as u64 <= MAX_BYTES)
                    .then(|| serde_json::from_str(&raw).ok())
                    .flatten()
            })
            .unwrap_or_default();
        values.prune(nova_config::now_secs());
        Mutex::new(values)
    })
}
pub(super) fn verified(identity: Option<&str>) -> bool {
    let Some(identity) = identity else {
        return false;
    };
    let values = cache().lock().unwrap();
    let now = nova_config::now_secs();
    values
        .0
        .get(identity)
        .is_some_and(|at| now >= *at && now - *at < TTL)
}
pub(super) fn remember(identity: &str) {
    if !valid_fingerprint(identity) {
        return;
    }
    let mut values = cache().lock().unwrap();
    let now = nova_config::now_secs();
    values.0.insert(identity.into(), now);
    values.prune(now);
    if let Ok(raw) = serde_json::to_vec(&*values) {
        // Disk latency never blocks the playback tick. A bounded mailbox drops
        // redundant snapshots when a write is already waiting.
        static WRITER: OnceLock<std::sync::mpsc::SyncSender<Vec<u8>>> = OnceLock::new();
        let writer = WRITER.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
            let directory = nova_config::app_cache_dir();
            std::thread::spawn(move || {
                while let Ok(mut raw) = rx.recv() {
                    while let Ok(newest) = rx.try_recv() {
                        raw = newest;
                    }
                    if std::fs::create_dir_all(&directory).is_ok() {
                        let temporary = directory.join("source-success-v1.json.tmp");
                        if std::fs::write(&temporary, raw).is_ok() {
                            let _ = std::fs::rename(
                                temporary,
                                directory.join("source-success-v1.json"),
                            );
                        }
                    }
                }
            });
            tx
        });
        let _ = writer.try_send(raw);
    }
}

/// A seek, pause, stall or suspension cannot establish a successful source.
#[derive(Default)]
pub(super) struct ProgressEvidence {
    previous: Option<(Instant, f64)>,
    advancing: Duration,
    recorded: bool,
}
impl ProgressEvidence {
    pub(super) fn interrupt(&mut self) {
        self.previous = None;
        self.advancing = Duration::ZERO;
    }
    pub(super) fn observe(&mut self, now: Instant, position: f64, playing: bool) -> bool {
        if self.recorded {
            return false;
        }
        if !playing || !position.is_finite() {
            self.interrupt();
            return false;
        }
        if let Some((at, previous)) = self.previous.replace((now, position)) {
            let elapsed = now.saturating_duration_since(at);
            let delta = position - previous;
            if elapsed <= Duration::from_secs(2)
                && delta > 0.001
                && delta <= elapsed.as_secs_f64() * 8.0 + 0.5
            {
                self.advancing += elapsed;
            } else {
                self.advancing = Duration::ZERO;
            }
        }
        if self.advancing >= Duration::from_secs(30) {
            self.recorded = true;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct RetryBudget {
    attempted: Vec<String>,
    original: Option<String>,
    pub(super) last_tick: Option<Instant>,
    pub(super) cancelled: bool,
}
impl RetryBudget {
    pub(super) fn exclude_original(&mut self, identity: &str) {
        self.original.get_or_insert_with(|| identity.into());
    }
    pub(super) fn accepts(&self, identity: &str) -> bool {
        self.original.as_deref() != Some(identity)
            && !self.cancelled
            && self.attempted.len() < 2
            && !self.attempted.iter().any(|id| id == identity)
    }
    pub(super) fn attempt(&mut self, identity: &str) -> bool {
        if !self.accepts(identity) {
            return false;
        }
        self.attempted.push(identity.into());
        true
    }
}

pub(super) fn resume_in_bounds(position: f64, duration: f64) -> bool {
    position.is_finite()
        && position >= 0.0
        && duration.is_finite()
        && duration > 0.0
        && position < duration
}

#[derive(Clone)]
pub(super) struct Timeline {
    pub(super) position: f64,
    pub(super) duration: f64,
    pub(super) source: Option<String>,
}
impl Timeline {
    pub(super) fn can_resume_without_question(&self, source: Option<&str>, duration: f64) -> bool {
        self.source
            .as_deref()
            .zip(source)
            .is_some_and(|(old, new)| old == new)
            && self.duration.is_finite()
            && self.duration > 0.0
            && resume_in_bounds(self.position, duration)
            && (self.duration - duration).abs() <= 5.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn success_requires_sustained_real_progress_and_never_seek_or_background_time() {
        let start = Instant::now();
        let mut evidence = ProgressEvidence::default();
        for second in 0..30 {
            assert!(!evidence.observe(start + Duration::from_secs(second), second as f64, true));
        }
        assert!(evidence.observe(start + Duration::from_secs(30), 30., true));
        assert!(!evidence.observe(start + Duration::from_secs(31), 31., true));
        let mut interrupted = ProgressEvidence::default();
        for second in 0..30 {
            assert!(!interrupted.observe(start + Duration::from_secs(second), second as f64, true));
        }
        interrupted.interrupt(); // Even a small explicit seek restarts evidence.
        for second in 30..60 {
            assert!(!interrupted.observe(start + Duration::from_secs(second), second as f64, true));
        }
        assert!(interrupted.observe(start + Duration::from_secs(60), 60., true));
        let mut evidence = ProgressEvidence::default();
        for second in 0..90 {
            assert!(!evidence.observe(
                start + Duration::from_secs(second),
                second as f64 * 100.,
                true
            ));
        }
        assert!(!evidence.observe(start + Duration::from_secs(150), 9001., true));
        evidence.observe(start + Duration::from_secs(151), 9002., false);
        for second in 152..181 {
            assert!(!evidence.observe(start + Duration::from_secs(second), second as f64, true));
        }
    }
    #[test]
    fn recovery_never_repeats_an_alternative_or_exceeds_two_attempts() {
        let mut budget = RetryBudget::default();
        budget.exclude_original("original");
        assert!(!budget.attempt("original"));
        assert!(budget.attempt("a"));
        budget.exclude_original("a"); // A replacement must not reset the origin.
        assert!(!budget.accepts("original"));
        assert!(!budget.attempt("a"));
        assert!(budget.attempt("b"));
        assert!(!budget.attempt("c"));
        let mut cancelled = RetryBudget {
            cancelled: true,
            ..Default::default()
        };
        assert!(!cancelled.attempt("a"));
    }
    #[test]
    fn a_shorter_replacement_cannot_resume_at_or_beyond_its_end() {
        assert!(!resume_in_bounds(6000., 3600.));
        assert!(!resume_in_bounds(3600., 3600.));
        assert!(!resume_in_bounds(6000., 0.));
        assert!(!resume_in_bounds(6000., f64::NAN));
        assert!(resume_in_bounds(6000., 7200.));
    }

    #[test]
    fn equal_durations_do_not_prove_corresponding_editions() {
        let timeline = Timeline {
            position: 600.,
            duration: 3600.,
            source: Some("torrent:file".into()),
        };
        assert!(!timeline.can_resume_without_question(Some("other:file"), 3600.));
        assert!(!timeline.can_resume_without_question(None, 3600.));
        assert!(!timeline.can_resume_without_question(Some("torrent:file"), 3800.));
        assert!(timeline.can_resume_without_question(Some("torrent:file"), 3601.));
    }
    #[test]
    fn cache_rejects_urls_expires_and_evicts_oldest() {
        let now = TTL + 1000;
        let mut values = Successes::default();
        values.0.insert("https://host/?token=secret".into(), now);
        for n in 0..140 {
            values.0.insert(format!("stream-{n:064x}"), now - n);
        }
        values.0.insert(format!("stream-{:064x}", 999), 0);
        values.prune(now);
        assert_eq!(values.0.len(), LIMIT);
        assert!(!values.0.contains_key("https://host/?token=secret"));
        assert!(values.0.contains_key(&format!("stream-{:064x}", 0)));
        assert!(!values.0.contains_key(&format!("stream-{:064x}", 139)));
    }
}

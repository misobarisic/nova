//! Device-local playback evidence and conservative recovery policy.
//! No resolved links, credentials or inferred hardware capability are retained.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    sync::{Arc, Mutex, OnceLock, mpsc},
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

/// Keep one pending cumulative snapshot; the bounded channel only wakes the
/// writer. A full wake channel must not discard newer success evidence.
struct SuccessWriter {
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    wake: mpsc::SyncSender<()>,
}
impl SuccessWriter {
    fn spawn(
        mut persist: impl FnMut(Vec<u8>) + Send + 'static,
    ) -> (Self, std::thread::JoinHandle<()>) {
        let latest = Arc::new(Mutex::new(None));
        let pending = latest.clone();
        let (wake, receiver) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            while receiver.recv().is_ok() {
                // Release the slot lock before disk I/O or another submission
                // would wait for the very write this mailbox separates out.
                let raw = pending.lock().unwrap().take();
                if let Some(raw) = raw {
                    persist(raw);
                }
            }
        });
        (Self { latest, wake }, worker)
    }

    fn submit(&self, raw: Vec<u8>) {
        *self.latest.lock().unwrap() = Some(raw);
        let _ = self.wake.try_send(());
    }
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
        // Serialization stays ordered with the cumulative cache update. Only
        // obsolete pending snapshots are replaced; disk I/O stays off this tick.
        static WRITER: OnceLock<SuccessWriter> = OnceLock::new();
        let writer = WRITER.get_or_init(|| {
            let directory = nova_config::app_cache_dir();
            let (writer, _worker) = SuccessWriter::spawn(move |raw| {
                if std::fs::create_dir_all(&directory).is_ok() {
                    let temporary = directory.join("source-success-v1.json.tmp");
                    if std::fs::write(&temporary, raw).is_ok() {
                        let _ =
                            std::fs::rename(temporary, directory.join("source-success-v1.json"));
                    }
                }
            });
            writer
        });
        writer.submit(raw);
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
    fn blocked_success_writer_keeps_latest_cumulative_snapshot_without_another_submit() {
        let (recorded, records) = mpsc::channel();
        let gate = Arc::new(std::sync::Barrier::new(2));
        let worker_gate = gate.clone();
        let (writer, worker) = SuccessWriter::spawn(move |raw| {
            let first = raw == b"A";
            recorded.send(raw).unwrap();
            if first {
                worker_gate.wait();
            }
        });
        writer.submit(b"A".to_vec());
        assert_eq!(records.recv_timeout(Duration::from_secs(5)).unwrap(), b"A");
        // A is in progress, B fills the wake channel, and C must replace B
        // even though its wakeup cannot be enqueued. Neither submission waits
        // on the blocked sink because it never holds the pending-slot lock.
        writer.submit(b"AB".to_vec());
        writer.submit(b"ABC".to_vec());
        assert_eq!(writer.latest.lock().unwrap().as_deref(), Some(&b"ABC"[..]));
        drop(writer);
        gate.wait();
        assert_eq!(
            records.recv_timeout(Duration::from_secs(5)).unwrap(),
            b"ABC"
        );
        worker.join().unwrap();
        assert!(records.try_recv().is_err());
    }

    #[test]
    fn wake_after_pending_slot_take_is_not_lost() {
        let (taken, observed) = mpsc::channel();
        let gate = Arc::new(std::sync::Barrier::new(2));
        let worker_gate = gate.clone();
        let (writer, worker) = SuccessWriter::spawn(move |raw| {
            taken.send(raw).unwrap();
            worker_gate.wait();
        });
        writer.submit(b"A".to_vec());
        for raw in [b"A".as_slice(), b"AB", b"ABC"] {
            assert_eq!(observed.recv_timeout(Duration::from_secs(5)).unwrap(), raw);
            assert!(writer.latest.lock().unwrap().is_none());
            // The sink is blocked after taking the slot; its next wake must
            // still be retained even if it arrives before the next recv().
            if raw != b"ABC" {
                let next = if raw == b"A" {
                    b"AB".as_slice()
                } else {
                    b"ABC"
                };
                writer.submit(next.to_vec());
            }
            gate.wait();
        }
        drop(writer);
        worker.join().unwrap();
        assert!(observed.try_recv().is_err());
    }

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

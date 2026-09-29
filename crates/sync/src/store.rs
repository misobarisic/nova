//! Durable record store: a two-level `domain -> key -> Record` map persisted
//! to the app's redb KV store (via `nova-storage`) as a single JSON blob, plus
//! the hybrid logical clock that stamps local writes.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::hlc::Hlc;
use crate::merge::{resolve, Record, Version};

/// Version digest exchanged at the start of a sync: every `(domain, key)`
/// with its version, no values. Both sides use it to compute what to send.
pub type Digest = BTreeMap<String, BTreeMap<String, Version>>;

/// A record this side wants to push to a peer.
pub struct Outbound {
    pub domain: String,
    pub key: String,
    pub record: Record,
}

/// Legacy whole-store blob key. Read once to migrate an old install, then
/// removed; new builds persist one row per record instead (below).
const LEGACY_RECORDS_KEY: &str = "sync:records";
/// Per-record row key prefix. The domain is length-prefixed so keys (which may
/// contain `:` and any separator we might pick) can never be misparsed:
/// `srec:{domain_byte_len}:{domain}{key}`.
const RECORD_PREFIX: &str = "srec:";
/// Key holding the persisted hybrid logical clock.
const CLOCK_KEY: &str = "sync:records:hlc";
/// How long a tombstone is retained before GC (covers offline peers).
pub const TOMBSTONE_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// Encode the persistence row key for `(domain, key)`.
fn record_row_key(domain: &str, key: &str) -> String {
    format!("{RECORD_PREFIX}{}:{domain}{key}", domain.len())
}

/// Decode a row key back into `(domain, key)`; `None` if malformed.
fn parse_record_row_key(row: &str) -> Option<(String, String)> {
    let rest = row.strip_prefix(RECORD_PREFIX)?;
    let (len, after) = rest.split_once(':')?;
    let dlen: usize = len.parse().ok()?;
    if after.len() < dlen {
        return None;
    }
    let (domain, key) = after.split_at(dlen);
    Some((domain.to_string(), key.to_string()))
}

/// How a tombstone may be collected. Computed from the per-peer ack map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AckFloor {
    /// No peers at all: plain TTL expiry applies.
    NoPeers,
    /// Some peer has never acknowledged: retain every tombstone.
    Blocked,
    /// Every peer has acknowledged up to this HLC; tombstones at or below it
    /// are safe to collect once past the TTL.
    At(Hlc),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    domains: BTreeMap<String, BTreeMap<String, Record>>,
    /// Hybrid logical clock; persisted so monotonicity survives restarts.
    #[serde(default)]
    hlc: Hlc,
    /// Rows changed since the last `save`, so persistence writes only what
    /// moved instead of the whole store. Runtime-only (not serialized).
    #[serde(skip)]
    dirty: BTreeSet<(String, String)>,
}

impl Store {
    /// Load from the KV store (empty when absent).
    ///
    /// A legacy whole-store `sync:records` blob is migrated to per-record rows
    /// once, then removed. A pre-HLC blob (second-resolution timestamps, no
    /// counter) is upgraded to the HLC layout first.
    ///
    /// Unreadable data is *not* silently discarded: the legacy blob (or an
    /// individual unreadable record row) is copied to a timestamped backup key
    /// and logged, so a parse failure can be recovered and never loses the
    /// peer list/history without a trace.
    pub fn load() -> Self {
        if let Some(raw) = nova_storage::get_str(LEGACY_RECORDS_KEY) {
            return match legacy_to_store(&raw) {
                Ok(mut store) => {
                    // Write every record as a row, then drop the blob so the
                    // migration runs once.
                    store.mark_all_dirty();
                    store.save();
                    nova_storage::remove(LEGACY_RECORDS_KEY);
                    eprintln!("nova sync: migrated sync:records to per-record rows");
                    store
                }
                Err(reason) => {
                    let backup =
                        format!("{LEGACY_RECORDS_KEY}.corrupt.{}", crate::now_secs());
                    nova_storage::set_str(&backup, &raw);
                    eprintln!(
                        "nova sync: stored records are unreadable ({reason}); backed up to {backup}"
                    );
                    Self::load_rows()
                }
            };
        }
        Self::load_rows()
    }

    /// Rebuild the store from the per-record rows.
    fn load_rows() -> Self {
        let mut store = Self::default();
        if let Some(hlc) = nova_storage::get_str(CLOCK_KEY)
            .and_then(|s| serde_json::from_str::<Hlc>(&s).ok())
        {
            store.hlc = hlc;
        }
        for (row, value) in nova_storage::scan_prefix(RECORD_PREFIX) {
            let Some((domain, key)) = parse_record_row_key(&row) else {
                eprintln!("nova sync: skipping malformed record row {row:?}");
                continue;
            };
            match serde_json::from_str::<Record>(&value) {
                Ok(record) => {
                    store
                        .domains
                        .entry(domain)
                        .or_default()
                        .insert(key, record);
                }
                Err(e) => {
                    let backup = format!("{row}.corrupt.{}", crate::now_secs());
                    nova_storage::set_str(&backup, &value);
                    eprintln!("nova sync: record row {row:?} unreadable ({e}); backed up to {backup}");
                }
            }
        }
        store.reconcile_clock();
        store
    }

    /// Mark every live record for rewrite (used by the legacy migration).
    fn mark_all_dirty(&mut self) {
        for (domain, keys) in &self.domains {
            for key in keys.keys() {
                self.dirty.insert((domain.clone(), key.clone()));
            }
        }
    }

    /// The raw record for `(domain, key)`, tombstone included. Used by the
    /// membership code to tell "absent" from "deleted".
    pub fn record(&self, domain: &str, key: &str) -> Option<&Record> {
        self.domains.get(domain).and_then(|m| m.get(key))
    }

    /// Persist every record changed since the last save (plus the clock) as
    /// one batched transaction, instead of rewriting the whole store.
    pub fn save(&mut self) {
        let dirty = std::mem::take(&mut self.dirty);
        let mut batch: Vec<(String, Option<String>)> = Vec::with_capacity(dirty.len() + 1);
        for (domain, key) in dirty {
            let row = record_row_key(&domain, &key);
            match self.domains.get(&domain).and_then(|m| m.get(&key)) {
                Some(record) => match serde_json::to_string(record) {
                    Ok(value) => batch.push((row, Some(value))),
                    Err(e) => eprintln!("nova sync: serialize record {domain}/{key}: {e}"),
                },
                // Removed (e.g. an acked tombstone collected by GC).
                None => batch.push((row, None)),
            }
        }
        // The clock moves on every set/apply, including an apply that wins no
        // record; persist it so monotonicity survives restarts.
        match serde_json::to_string(&self.hlc) {
            Ok(hlc) => batch.push((CLOCK_KEY.to_string(), Some(hlc))),
            Err(e) => eprintln!("nova sync: serialize clock: {e}"),
        }
        nova_storage::write_batch(&batch);
    }

    /// Ensure the clock is at least as new as every stored record, so a local
    /// write always supersedes what we already hold (and the digest snapshot
    /// covers every record).
    fn reconcile_clock(&mut self) {
        let mut max = self.hlc;
        for keys in self.domains.values() {
            for record in keys.values() {
                let hlc = record.version.hlc();
                if hlc.newer_than(max) {
                    max = hlc;
                }
            }
        }
        self.hlc = max;
    }

    /// Record a local write. `observed_secs` is the caller's observation time
    /// (0 means "stamp now"); it is used only as a lower bound on the HLC, so
    /// a local edit always supersedes the last known version. Returns true when
    /// the value actually changed (a no-op re-notify doesn't tick the clock, so
    /// restarts don't re-push unchanged data).
    pub fn set(
        &mut self,
        domain: &str,
        key: &str,
        value: Option<String>,
        observed_secs: u64,
        dev: u64,
    ) -> bool {
        if let Some(cur) = self.domains.get(domain).and_then(|m| m.get(key))
            && cur.value == value
        {
            return false;
        }
        let hlc = self
            .hlc
            .tick(crate::now_ms(), observed_secs.saturating_mul(1000));
        let deleted = value.is_none();
        let record = Record {
            value,
            version: Version::from_hlc(hlc, dev, deleted),
        };
        self.domains
            .entry(domain.to_string())
            .or_default()
            .insert(key.to_string(), record);
        self.dirty.insert((domain.to_string(), key.to_string()));
        true
    }

    /// Apply a record received from a peer. Returns true when it superseded
    /// the local version. The remote clock is folded into ours first so a
    /// later local write always sorts after it (causality).
    pub fn apply(&mut self, domain: &str, key: &str, remote: Record) -> bool {
        self.hlc.observe(remote.version.hlc(), crate::now_ms());
        let slot = self.domains.entry(domain.to_string()).or_default();
        match resolve(slot.get(key), &remote) {
            Some(record) => {
                slot.insert(key.to_string(), record);
                self.dirty.insert((domain.to_string(), key.to_string()));
                true
            }
            None => false,
        }
    }

    /// Digest plus the clock reading it was taken at. The ack for an exchange
    /// is this HLC: every record in the digest has a version at or below it.
    pub fn snapshot(&self) -> (Digest, Hlc) {
        (self.digest(), self.hlc)
    }

    pub fn digest(&self) -> Digest {
        self.domains
            .iter()
            .map(|(domain, keys)| {
                (
                    domain.clone(),
                    keys.iter().map(|(k, r)| (k.clone(), r.version)).collect(),
                )
            })
            .collect()
    }

    /// Records this side should send to a peer: every record the peer lacks
    /// or has an older version of.
    pub fn outbound(&self, peer: &Digest) -> Vec<Outbound> {
        let mut out = Vec::new();
        for (domain, keys) in &self.domains {
            let peer_keys = peer.get(domain);
            for (key, record) in keys {
                let send = match peer_keys.and_then(|m| m.get(key)) {
                    Some(version) => record.version.newer_than(version),
                    None => true,
                };
                if send {
                    out.push(Outbound {
                        domain: domain.clone(),
                        key: key.clone(),
                        record: record.clone(),
                    });
                }
            }
        }
        out
    }

    /// Live values for a domain (tombstones omitted), for the app to
    /// materialize its own types from.
    pub fn records(&self, domain: &str) -> Vec<(String, String)> {
        self.domains
            .get(domain)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, r)| r.value.clone().map(|v| (k.clone(), v)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Domains that currently hold at least one record (live or tombstone).
    pub fn domains(&self) -> Vec<String> {
        self.domains.keys().cloned().collect()
    }

    /// Drop tombstones older than `max_age_ms`, but never one that a peer has
    /// not acknowledged. `floor` comes from the per-peer ack map: `At(h)` means
    /// every current peer has seen records up to `h`, so a tombstone with
    /// `version <= h` is safe; `Blocked` retains everything; `NoPeers` applies
    /// plain TTL expiry. Live records are always kept.
    pub fn gc(&mut self, now_ms: u64, max_age_ms: u64, floor: AckFloor) {
        if floor == AckFloor::Blocked {
            return;
        }
        let mut removed: Vec<(String, String)> = Vec::new();
        for (domain, keys) in self.domains.iter_mut() {
            keys.retain(|key, r| {
                if !r.is_deleted() || now_ms.saturating_sub(r.version.ts) <= max_age_ms {
                    return true;
                }
                let keep = match floor {
                    AckFloor::NoPeers => false,
                    // Keep when the tombstone is newer than the ack floor.
                    AckFloor::At(h) => r.version.hlc().newer_than(h),
                    AckFloor::Blocked => true,
                };
                if !keep {
                    // Persist the removal (drops the row) on the next save.
                    removed.push((domain.clone(), key.clone()));
                }
                keep
            });
        }
        self.domains.retain(|_, keys| !keys.is_empty());
        self.dirty.extend(removed);
    }
}

/// Parse a legacy whole-store blob into a `Store`, applying the pre-HLC
/// migration. Errors are returned as a short reason for the quarantine log.
fn legacy_to_store(raw: &str) -> Result<Store, &'static str> {
    let mut value = serde_json::from_str::<serde_json::Value>(raw).map_err(|_| "invalid JSON")?;
    migrate_legacy(&mut value);
    let mut store = serde_json::from_value::<Store>(value).map_err(|_| "unexpected schema")?;
    store.reconcile_clock();
    Ok(store)
}

/// One-time migration of a pre-HLC `sync:records` blob: records have
/// second-resolution `ts` and no `counter`. Scale timestamps to milliseconds
/// and add a zero counter. Returns true when anything changed.
fn migrate_legacy(value: &mut serde_json::Value) -> bool {
    let Some(domains) = value.get_mut("domains").and_then(|d| d.as_object_mut()) else {
        return false;
    };
    let mut migrated = false;
    for keys in domains.values_mut() {
        let Some(keys) = keys.as_object_mut() else {
            continue;
        };
        for record in keys.values_mut() {
            let Some(version) = record.get_mut("version").and_then(|v| v.as_object_mut()) else {
                continue;
            };
            if !version.contains_key("counter") {
                version.insert("counter".to_string(), serde_json::json!(0));
                let ts = version.get("ts").and_then(|t| t.as_u64()).unwrap_or(0);
                version.insert("ts".to_string(), serde_json::json!(ts.saturating_mul(1000)));
                migrated = true;
            }
            version
                .entry("deleted")
                .or_insert(serde_json::json!(false));
        }
    }
    migrated
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV_A: u64 = 10;
    const DEV_B: u64 = 20;

    #[test]
    fn record_row_key_round_trips_awkward_keys() {
        for (domain, key) in [
            ("library", "tt123"),
            ("presence", "abc\x01def"),
            ("weird:domain", "key:with:colons"),
            ("d", ""),
            ("", "just-key"),
        ] {
            let row = record_row_key(domain, key);
            assert_eq!(
                parse_record_row_key(&row),
                Some((domain.to_string(), key.to_string())),
                "round trip failed for {domain:?}/{key:?} via {row:?}"
            );
        }
        assert_eq!(parse_record_row_key("nope"), None);
        assert_eq!(parse_record_row_key("srec:zz:x"), None);
        assert_eq!(parse_record_row_key("srec:9:short"), None);
    }

    #[test]
    fn legacy_blob_parses_and_migrates_pre_hlc() {
        // Seconds-resolution `ts`, no `counter`: migrated to ms + counter.
        let raw = r#"{"domains":{"library":{"a":{"value":"1","version":{"ts":1700000000,"dev":7,"deleted":false}}}},"hlc":{"physical_ms":1700000000000,"counter":0}}"#;
        let store = legacy_to_store(raw).unwrap();
        let v = store.digest()["library"]["a"];
        assert_eq!(v.ts, 1_700_000_000_000);
        assert_eq!(v.counter, 0);
        assert!(!v.deleted);
        // Unreadable input is reported, not panicked on.
        assert!(legacy_to_store("{not json").is_err());
        assert!(legacy_to_store("\"a string\"").is_err());
    }

    #[test]
    fn set_skips_unchanged_values() {
        let mut s = Store::default();
        assert!(s.set("library", "a", Some("1".into()), 100, DEV_A));
        assert!(!s.set("library", "a", Some("1".into()), 200, DEV_A));
        assert!(s.set("library", "a", Some("2".into()), 200, DEV_A));
    }

    #[test]
    fn local_set_is_monotonic_even_with_stale_clock() {
        let mut s = Store::default();
        s.set("d", "k", Some("a".into()), 0, DEV_A);
        let first = s.digest()["d"]["k"];
        // A later local write with an older caller timestamp must still win.
        s.set("d", "k", Some("b".into()), 1, DEV_A);
        assert!(s.digest()["d"]["k"].newer_than(&first));
    }

    #[test]
    fn outbound_only_includes_missing_or_newer() {
        let mut a = Store::default();
        a.set("d", "same", Some("x".into()), 0, DEV_A);
        a.set("d", "newer", Some("y".into()), 0, DEV_A);
        let a_digest = a.digest();
        // Peer version map: "same" matches exactly, "newer" is stale.
        let mut keys = BTreeMap::new();
        keys.insert("same".to_string(), a_digest["d"]["same"]);
        keys.insert("newer".to_string(), Version::new(1, 0, DEV_B, false));
        let mut peer = Digest::new();
        peer.insert("d".to_string(), keys);

        let out = a.outbound(&peer);
        let sent: Vec<&str> = out.iter().map(|o| o.key.as_str()).collect();
        assert!(sent.contains(&"newer"));
        assert!(!sent.contains(&"same"));
    }

    #[test]
    fn apply_merges_and_hides_tombstones() {
        let mut a = Store::default();
        a.set("d", "k", Some("v".into()), 0, DEV_A);
        let rec = Record::present("remote".into(), Version::new(u64::MAX, 0, DEV_B, false));
        assert!(a.apply("d", "k", rec));
        assert_eq!(a.records("d"), vec![("k".to_string(), "remote".to_string())]);
        let del = Record::tombstone(Version::new(u64::MAX, 0, DEV_B, true));
        // Same (ts, counter, dev, deleted=false vs true) tie: tombstone ranks
        // above, so a tombstone with an equal version still wins.
        assert!(a.apply("d", "k", del));
        assert!(a.records("d").is_empty());
    }

    #[test]
    fn gc_drops_old_acked_tombstones() {
        let mut s = Store::default();
        s.set("d", "k", Some("v".into()), 0, DEV_A);
        s.set("d", "k", None, 0, DEV_A);
        let version = s.digest()["d"]["k"];
        let now = version.ts + TOMBSTONE_TTL_MS + 1;
        // Older than the TTL and every peer acked at/after it, so it can go.
        s.gc(now, TOMBSTONE_TTL_MS, AckFloor::At(version.hlc()));
        assert!(s.domains().is_empty());
    }

    #[test]
    fn gc_retains_tombstone_not_yet_acked_by_all_peers() {
        let mut s = Store::default();
        s.set("d", "k", Some("v".into()), 0, DEV_A);
        s.set("d", "k", None, 0, DEV_A);
        let version = s.digest()["d"]["k"];
        let now = version.ts + TOMBSTONE_TTL_MS + 1;
        // A peer last synced just before the deletion: dropping it could let
        // that device resurrect the record.
        let before = Hlc::new(version.ts.saturating_sub(1), version.counter);
        s.gc(now, TOMBSTONE_TTL_MS, AckFloor::At(before));
        assert!(s.domains().contains(&"d".to_string()));
        assert!(s.records("d").is_empty());
        // A peer that never synced likewise blocks collection.
        s.gc(now, TOMBSTONE_TTL_MS, AckFloor::Blocked);
        assert!(s.domains().contains(&"d".to_string()));
    }

    #[test]
    fn gc_keeps_fresh_tombstones_and_live_records() {
        let mut s = Store::default();
        s.set("d", "live", Some("v".into()), 0, DEV_A);
        s.set("d", "gone", None, 0, DEV_A);
        let version = s.digest()["d"]["gone"];
        // Within the TTL window: retained even though fully acked.
        s.gc(version.ts + 1, TOMBSTONE_TTL_MS, AckFloor::NoPeers);
        assert_eq!(s.records("d"), vec![("live".to_string(), "v".to_string())]);
        assert!(s.domains().contains(&"d".to_string()));
        // Past the TTL with no peers: the tombstone goes, the live record stays.
        s.gc(
            version.ts + TOMBSTONE_TTL_MS + 1,
            TOMBSTONE_TTL_MS,
            AckFloor::NoPeers,
        );
        assert_eq!(s.records("d"), vec![("live".to_string(), "v".to_string())]);
    }

    #[test]
    fn migrate_legacy_scales_seconds_and_adds_counter() {
        // A pre-HLC blob: second timestamps, no counter, no clock.
        let mut value = serde_json::json!({
            "domains": {
                "library": {
                    "a": { "value": "x", "version": { "ts": 1_700_000_000u64, "dev": 7, "deleted": false } }
                }
            }
        });
        assert!(migrate_legacy(&mut value));
        let store: Store = serde_json::from_value(value).unwrap();
        let v = store.digest()["library"]["a"];
        assert_eq!(v.ts, 1_700_000_000_000);
        assert_eq!(v.counter, 0);
        // Idempotent: a second pass changes nothing.
        let mut value2 = serde_json::to_value(&store).unwrap();
        assert!(!migrate_legacy(&mut value2));
    }
}

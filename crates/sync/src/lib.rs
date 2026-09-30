//! Cross-device sync over iroh.
//!
//! The engine owns an iroh [`Endpoint`] (on its own tokio runtime, like
//! `nova-torrent`) and a generic record store: `domain -> key -> value` with a
//! hybrid-logical-clock version per record and last-write-wins merge (see
//! [`merge`] and [`hlc`]). Peers dial each other by endpoint id and exchange
//! only the records the other side is missing or has stale (see [`protocol`]);
//! the wire format is postcard.
//!
//! The crate is intentionally unaware of the app's data model. The app
//! decomposes its state into [`SyncEngine::notify`] calls and rebuilds its own
//! types from [`SyncEngine::records`]; remote changes are announced through a
//! callback so the app can marshal them onto the UI thread.
//!
//! The mesh's own membership lives in the synced `peers` domain, so the list
//! of paired endpoints propagates by the same anti-entropy exchange as any
//! other record (additions and removals alike). Deletions are tombstones, and
//! a tombstone is only reclaimed once every current peer has acknowledged a
//! sync since it was created (`sync:peer_acks`, local only), so a device that
//! was offline for any length can never resurrect a deleted record.
//!
//! Sync is opt-in: nothing binds a socket until the app reads
//! [`read_settings`], sees `enabled`, and calls [`install`]/[`SyncEngine::setup`].

mod frame;
mod hlc;
mod local;
mod merge;
mod pair;
mod progress;
mod protocol;
mod store;
mod ticket;
pub use local::{commit_snapshot, local_device, local_store, prepare_snapshot, preserve_unknown};

pub use hlc::Hlc;
pub use merge::{Record, Version};
pub use pair::{ALPN as PAIR_ALPN, IncomingPair, Invite, PairCallback, PairEvent, PairHandler};
pub use protocol::ALPN;
pub use store::{AckFloor, Store};
pub use ticket::{decode_ticket, encode_ticket};

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
pub use anyhow::Result;
use iroh::endpoint::Connection;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use serde::{Deserialize, Serialize};

use protocol::Handler;

pub(crate) use nova_config::{now_ms, now_secs};

/// `sync:identity` — hex-encoded ed25519 secret key (stable endpoint id).
const IDENTITY_KEY: &str = "sync:identity";
/// `sync:peers` — JSON list of user-added peer endpoint ids.
const PEERS_KEY: &str = "sync:peers";
/// `sync:peer_names` — JSON map of peer endpoint id -> friendly device name.
const PEER_NAMES_KEY: &str = "sync:peer_names";
/// `sync:settings` — JSON [`SyncSettings`].
const SETTINGS_KEY: &str = "sync:settings";
/// `sync:peer_acks` — JSON map of peer endpoint id -> last successful sync
/// (unix secs). Local only: it gates tombstone GC, it is not replicated.
const PEER_ACKS_KEY: &str = "sync:peer_acks";

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncSettings {
    /// Opt-in master switch. Off means no endpoint is bound.
    #[serde(default)]
    pub enabled: bool,
    /// Interval between background syncs, in seconds.
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    /// When true, a periodic `JobScheduler` job keeps syncing while the app is
    /// closed (Android, best effort). Local only, never synced.
    #[serde(default = "default_true")]
    pub background_enabled: bool,
    /// Friendly device name shown in pairing prompts. Empty means "not set
    /// yet"; the app fills it with a detected name.
    #[serde(default)]
    pub device_name: String,
    /// When true, incoming pairing requests wait for the user to accept.
    #[serde(default)]
    pub require_confirmation: bool,
    /// When false, skip LAN-side network activity: no UPnP/PCP/NAT-PMP
    /// gateway probing (SSDP multicast) and no mDNS LAN discovery. For
    /// faulty Wi-Fi chips that choke on multicast traffic. Device-local,
    /// like the rest of these settings. Defaults to on (current behavior).
    #[serde(default = "default_true")]
    pub enable_local_discovery: bool,
}

fn default_interval_secs() -> u64 {
    // One minute: responsive enough while the app is open. The poll loop
    // clamps this while backgrounded-but-alive, and the closed-app cadence is
    // the JobScheduler job (15 min floor), not this value.
    60
}

fn default_true() -> bool {
    true
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: default_interval_secs(),
            background_enabled: true,
            device_name: String::new(),
            require_confirmation: false,
            enable_local_discovery: true,
        }
    }
}

/// Whether the app is currently foregrounded. The poll loop uses the user's
/// interval while foregrounded and clamps it while backgrounded-but-alive, so a
/// long download (whose foreground service keeps the process alive) does not
/// hammer the network every 30 s for hours.
static FOREGROUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Record whether the app is in the foreground (Android lifecycle). Desktop
/// stays `true`.
pub fn set_foreground(foreground: bool) {
    FOREGROUND.store(foreground, std::sync::atomic::Ordering::Relaxed);
    if let Some(engine) = engine() {
        engine.cadence_notify.notify_one();
    }
}

/// Read the persisted sync settings (defaults = disabled).
pub fn read_settings() -> SyncSettings {
    nova_storage::get_str(SETTINGS_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the sync settings. Does not start/stop the engine.
pub fn write_settings(settings: &SyncSettings) {
    match serde_json::to_string(settings) {
        Ok(s) => nova_storage::set_str(SETTINGS_KEY, &s),
        Err(e) => eprintln!("nova sync: serialize settings: {e}"),
    }
    if let Some(engine) = engine() {
        engine.cadence_notify.notify_one();
    }
}

/// The name to advertise for this device: the configured override, or empty
/// when unset (callers may substitute a placeholder for display, but an empty
/// name must never overwrite a name learned for a peer).
pub(crate) fn effective_device_name(settings: &SyncSettings) -> String {
    settings.device_name.trim().to_string()
}

// ---------------------------------------------------------------------------
// Status + remote callback
// ---------------------------------------------------------------------------

/// Snapshot of the engine state for the Settings UI.
#[derive(Clone, Debug, Default)]
pub struct SyncStatus {
    /// A sync exchange is in flight.
    pub syncing: bool,
    /// When the current in-flight pass started (epoch secs, 0 = not syncing).
    /// Used to expire a stuck `syncing` flag.
    pub syncing_since: u64,
    /// Seconds since the epoch of the last completed attempt (0 = never).
    pub last_sync_secs: u64,
    /// Last error, cleared on the next successful sync.
    pub last_error: Option<String>,
    /// Number of configured peers.
    pub peer_count: usize,
    /// Monotonic count of completed worker wake-ups (including passes that had
    /// no eligible peer). Lets a one-shot caller — the Android background job —
    /// wait for "a pass attempt finished" without inferring it from the
    /// transient `syncing` flag.
    pub pass_count: u64,
    /// Completed durable exchanges, not merely worker wakes.
    pub success_count: u64,
    pub peer_attempts: HashMap<String, PeerAttempt>,
}

#[derive(Clone, Debug, Default)]
pub struct PeerAttempt {
    pub in_flight: bool,
    pub last_attempt_secs: u64,
    pub last_result_secs: u64,
    pub last_success_secs: u64,
    pub last_error: Option<String>,
    pub retry_after_secs: u64,
    pub duration_ms: u64,
}

/// Called after a sync applies remote records, with the domains that changed.
/// Runs on the tokio runtime thread; the app must marshal to the UI thread.
pub type RemoteCallback = Arc<dyn Fn(Vec<String>) + Send + Sync>;

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

pub struct SyncEngine {
    rt: tokio::runtime::Runtime,
    endpoint: Endpoint,
    router: Mutex<Option<Router>>,
    handler: Arc<Handler>,
    pair: Arc<PairHandler>,
    store: Arc<Mutex<Store>>,
    peers: Arc<Mutex<Vec<String>>>,
    status: Arc<Mutex<SyncStatus>>,
    /// Live per-peer connections, reused across passes. A pass opens a fresh
    /// bi stream on the cached connection instead of paying a QUIC handshake
    /// every time; a closed or failed connection is evicted and redialed.
    conns: Arc<Mutex<HashMap<String, Connection>>>,
    /// Wakes the sync worker; coalesces overlapping triggers.
    sync_notify: Arc<tokio::sync::Notify>,
    /// Recompute the periodic deadline without requesting a pass.
    cadence_notify: Arc<tokio::sync::Notify>,
    /// One explicit recovery pass may bypass old failure health.
    force_attempt: Arc<AtomicBool>,
    /// At most one iroh path refresh is scheduled at a time.
    network_refreshing: Arc<AtomicBool>,
    device: u64,
    stop: Arc<AtomicBool>,
}

/// Per-peer failure state driving the backoff between passes.
#[derive(Default)]
struct PeerHealth {
    last_fail: Option<Instant>,
    fail_count: u32,
}

/// Max peers contacted concurrently within one pass.
const SYNC_CONCURRENCY: usize = 4;
/// Per-peer connect timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-peer exchange timeout (after connecting). Generous: a first sync of a
/// large library over a relay can take a while, and aborting drops the
/// connection under the peer mid-exchange.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(120);
/// Backoff after the first failure, doubling per consecutive failure.
const BACKOFF_BASE_SECS: u64 = 30;
/// Backoff ceiling.
const BACKOFF_MAX_SECS: u64 = 10 * 60;
/// Hard ceiling on a single pass. A pass is normally seconds; if it exceeds
/// this the worker aborts it and clears the syncing flag, so a wedged peer
/// cannot leave the UI stuck on "Syncing…".
const MAX_PASS_SECS: u64 = 8 * 60;
/// The UI stops reporting "syncing" once a pass has been in flight this long.
const MAX_SYNCING_SECS: u64 = MAX_PASS_SECS + 30;
/// Max back-to-back passes within one wake-up while the mesh is still
/// discovering peers. Each round dials peers learned in the previous one, so a
/// freshly paired device reaches the whole mesh in a single wake-up instead of
/// waiting for the next interval.
const MAX_PASS_ROUNDS: usize = 5;

/// Whether a peer is still within its backoff window and should be skipped.
fn should_skip(health: &Arc<Mutex<HashMap<String, PeerHealth>>>, id: &str, now: Instant) -> bool {
    let map = health.lock().unwrap_or_else(|e| e.into_inner());
    match map.get(id) {
        Some(state) if state.fail_count > 0 => {
            let shifts = (state.fail_count - 1).min(6);
            let backoff = BACKOFF_BASE_SECS
                .saturating_mul(1u64 << shifts)
                .min(BACKOFF_MAX_SECS);
            state.last_fail.is_some_and(|failed| {
                now.saturating_duration_since(failed) < Duration::from_secs(backoff)
            })
        }
        _ => false,
    }
}

fn record_failure(health: &Arc<Mutex<HashMap<String, PeerHealth>>>, id: &str) {
    let mut map = health.lock().unwrap_or_else(|e| e.into_inner());
    let state = map.entry(id.to_string()).or_default();
    state.last_fail = Some(Instant::now());
    state.fail_count = state.fail_count.saturating_add(1);
}

fn record_success(health: &Arc<Mutex<HashMap<String, PeerHealth>>>, id: &str) {
    let mut map = health.lock().unwrap_or_else(|e| e.into_inner());
    map.remove(id);
}

impl SyncEngine {
    /// A job owns this pass future, not the engine worker. Cancellation drops
    /// its JoinSet/writers without stopping an attached foreground owner.
    pub fn one_shot(&self, cancel: &AtomicBool, budget: Duration) -> OneShotOutcome {
        if cancel.load(Ordering::Acquire) || self.stop.load(Ordering::Relaxed) {
            return OneShotOutcome::Cancelled;
        }
        if budget.is_zero() {
            return OneShotOutcome::TimedOut;
        }
        let started = Instant::now();
        let outcome = self.rt.block_on(async {
            tokio::select! {
                outcome = run_pass(self.endpoint.clone(), self.handler.clone(), self.peers.clone(),
                    self.conns.clone(), Arc::new(Mutex::new(HashMap::new()))) => outcome,
                _ = tokio::time::sleep(budget) => OneShotOutcome::TimedOut,
                _ = async {
                    while !cancel.load(Ordering::Acquire) && !self.stop.load(Ordering::Relaxed) {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                } => OneShotOutcome::Cancelled,
            }
        });
        if std::env::var_os("NOVA_SYNC_DEBUG").is_some() {
            let class = match &outcome {
                OneShotOutcome::Completed => "completed",
                OneShotOutcome::Cancelled => "cancelled",
                OneShotOutcome::TimedOut => "timed_out",
                OneShotOutcome::Skipped => "skipped",
                OneShotOutcome::Failed(_) => "failed",
            };
            eprintln!(
                "[sync] trigger=one_shot outcome={class} duration_ms={}",
                started.elapsed().as_millis()
            );
        }
        outcome
    }
    /// Load identity/peers, bind the endpoint, start the accept router and the
    /// background sync loop. Requires `nova-storage` to be initialized.
    pub fn setup() -> Result<Self> {
        let secret = load_or_create_secret()?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .context("build sync runtime")?;

        let endpoint = rt
            .block_on(async {
                let local_discovery = read_settings().enable_local_discovery;
                let builder = Endpoint::builder(presets::N0)
                    .secret_key(secret)
                    .alpns(vec![ALPN.to_vec()]);
                // Gateway probing (UPnP/PCP/NAT-PMP, incl. SSDP multicast)
                // is on by default in iroh; skip it for faulty Wi-Fi.
                // This flag also gates any future mDNS LAN discovery.
                let builder = if local_discovery {
                    builder
                } else {
                    builder.portmapper_config(iroh::endpoint::PortmapperConfig::Disabled)
                };
                builder.bind().await
            })
            .context("bind iroh endpoint")?;

        let device = nova_config::fnv1a(endpoint.id().as_bytes());

        let store = local_store()?;
        let self_id = endpoint.id().to_string();
        {
            let now = now_secs();
            let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
            // One-time migration of the legacy peer list + names into the
            // synced `peers` domain (before the allowlist is derived below).
            let legacy = load_legacy_peers();
            if migrate_peers(&mut s, &legacy, now, device) {
                eprintln!(
                    "nova sync: migrated {} peer(s) into the peers domain",
                    legacy.len()
                );
            }
            s.save()?;
        }

        let peers = Arc::new(Mutex::new(allowlist_from_store(
            &store.lock().unwrap_or_else(|e| e.into_inner()),
            &self_id,
        )));
        // Per-peer ack times gate tombstone GC: a tombstone is only dropped
        // once every current peer has synced since it was created, so a device
        // that was offline for any length cannot resurrect a deleted record.
        let acks = Arc::new(Mutex::new(load_peer_acks()));
        {
            let acks_snapshot = acks.lock().map(|a| a.clone()).unwrap_or_default();
            let peer_list = peers.lock().map(|p| p.clone()).unwrap_or_default();
            let floor = ack_floor(&peer_list, &acks_snapshot);
            let now = now_ms();
            let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
            if s.gc(now, store::TOMBSTONE_TTL_MS, floor) > 0 {
                s.save()?;
            }
        }

        let status = Arc::new(Mutex::new(SyncStatus {
            peer_count: peers.lock().map(|p| p.len()).unwrap_or(0),
            ..Default::default()
        }));
        let on_remote: Arc<Mutex<Option<RemoteCallback>>> = Arc::new(Mutex::new(None));
        let handler = Arc::new(Handler::new(
            store.clone(),
            device,
            self_id,
            peers.clone(),
            status.clone(),
            on_remote,
            acks.clone(),
        ));

        // Pairing (invite tickets) shares the peers list and runs on the same
        // endpoint under a second ALPN.
        let invites = Arc::new(Mutex::new(pair::load_invites()));
        {
            // Drop expired invites at startup.
            let now = now_secs();
            let mut list = invites.lock().unwrap_or_else(|e| e.into_inner());
            list.retain(|i| i.expires_at > now);
            pair::save_invites(&list);
        }
        let on_pair = Arc::new(Mutex::new(None));
        let pending = Arc::new(Mutex::new(std::collections::HashMap::new()));
        // Created before the pair handler so pairing can wake the sync worker
        // immediately (see `PairHandler::set_sync_notify`).
        let sync_notify = Arc::new(tokio::sync::Notify::new());
        let pair_handler = Arc::new(PairHandler::new(
            endpoint.id().to_string(),
            device,
            store.clone(),
            peers.clone(),
            invites,
            on_pair,
            pending,
        ));
        pair_handler.set_sync_notify(sync_notify.clone());

        let router = rt.block_on(async {
            Router::builder(endpoint.clone())
                .accept(ALPN, handler.clone())
                .accept(pair::ALPN, pair_handler.clone())
                .accept(
                    protocol::REMOVE_ALPN,
                    std::sync::Arc::new(protocol::RemoveHandler::new(handler.clone())),
                )
                .spawn()
        });

        let stop = Arc::new(AtomicBool::new(false));
        let cadence_notify = Arc::new(tokio::sync::Notify::new());
        let force_attempt = Arc::new(AtomicBool::new(false));
        let health: Arc<Mutex<HashMap<String, PeerHealth>>> = Arc::new(Mutex::new(HashMap::new()));
        let conns: Arc<Mutex<HashMap<String, Connection>>> = Arc::new(Mutex::new(HashMap::new()));
        {
            // The worker runs one bounded-parallel pass per wake-up; triggers
            // that arrive mid-pass leave a `Notify` permit, so they queue a
            // single follow-up pass instead of being dropped.
            let endpoint = endpoint.clone();
            let handler = handler.clone();
            let peers = peers.clone();
            let health = health.clone();
            let conns = conns.clone();
            let sync_notify = sync_notify.clone();
            let stop = stop.clone();
            let force_attempt = force_attempt.clone();
            rt.spawn(sync_worker(
                endpoint,
                handler,
                peers,
                conns,
                health,
                sync_notify,
                stop,
                force_attempt,
            ));
        }
        {
            let endpoint = endpoint.clone();
            let handler = handler.clone();
            let status = status.clone();
            let on_pair = pair_handler.on_pair.clone();
            let sync_notify = sync_notify.clone();
            let stop = stop.clone();
            let cadence_notify = cadence_notify.clone();
            rt.spawn(interval_loop(
                endpoint,
                handler,
                status,
                on_pair,
                sync_notify,
                stop,
                cadence_notify,
            ));
        }

        Ok(Self {
            rt,
            endpoint,
            router: Mutex::new(Some(router)),
            handler,
            pair: pair_handler,
            store,
            peers,
            status,
            conns,
            sync_notify,
            cadence_notify,
            force_attempt,
            network_refreshing: Arc::new(AtomicBool::new(false)),
            device,
            stop,
        })
    }

    /// This device's endpoint id (share it with a peer to be added there).
    pub fn identity(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// Stable numeric device id used for LWW tie-breaks.
    pub fn device_id(&self) -> u64 {
        self.device
    }

    pub fn peers(&self) -> Vec<String> {
        self.peers.lock().map(|p| p.clone()).unwrap_or_default()
    }

    /// Add a peer by endpoint id (validated + normalized). Deduplicates. The
    /// peer is written to the synced `peers` domain, so it propagates to the
    /// rest of the mesh.
    pub fn add_peer(&self, id: &str) -> Result<()> {
        let parsed = EndpointId::from_str(id.trim()).context("invalid endpoint id")?;
        let normalized = parsed.to_string();
        peers_add(&self.store, &self.peers, &normalized, None, self.device)?;
        self.refresh_peer_count();
        // Dial the new peer (and let it introduce us to its peers) now.
        self.sync_now();
        Ok(())
    }

    /// Remove a peer. The tombstone replicates, so it disappears mesh-wide.
    pub fn remove_peer(&self, id: &str) {
        peers_remove(&self.store, &self.peers, id, self.device);
        if let Some(conn) = self
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
        {
            conn.close(protocol::REVOKED_CODE.into(), b"removed peer");
        }
        self.refresh_peer_count();
        // Push the tombstone out promptly instead of waiting for the interval.
        self.sync_now();
        // Best-effort: tell the peer directly so it drops us too rather than
        // keeping a peer whose syncs it will only ever see rejected.
        if let Ok(peer) = EndpointId::from_str(id) {
            let endpoint = self.endpoint.clone();
            let name = effective_device_name(&read_settings());
            self.rt.spawn(async move {
                protocol::notify_removed(&endpoint, EndpointAddr::from(peer), &name).await;
            });
        }
    }

    /// Friendly names for known peers (endpoint id -> device name). Peers
    /// added manually (not via pairing) have no entry.
    pub fn peer_names(&self) -> std::collections::HashMap<String, String> {
        peer_names_from_store(&self.store)
    }

    /// Last time `peer` was seen alive anywhere in the mesh (unix secs): the
    /// fresher of our direct ack clock and the mesh `presence` observations.
    /// `None` means never seen (or its sightings were all removed with it).
    pub fn peer_last_seen(&self, peer: &str) -> Option<u64> {
        let direct = self.handler.ack_secs(peer);
        let mesh = presence_newest(&self.store, peer);
        match (direct, mesh) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (direct, mesh) => direct.or(mesh),
        }
    }

    pub fn status(&self) -> SyncStatus {
        let mut snapshot = self.status.lock().map(|s| s.clone()).unwrap_or_default();
        snapshot.peer_count = self.peers.lock().map(|p| p.len()).unwrap_or(0);
        for attempt in snapshot.peer_attempts.values_mut() {
            if attempt.in_flight && now_secs().saturating_sub(attempt.last_attempt_secs) > 160 {
                attempt.last_error = Some("peer worker exceeded its attempt deadline".to_string());
            }
        }
        // Watchdog: if a pass has been "syncing" for implausibly long, stop
        // reporting it so the UI can't get stuck on "Syncing…" forever. The
        // pass itself is bounded in `sync_worker`; this covers the status.
        if snapshot.syncing
            && snapshot.syncing_since != 0
            && now_secs().saturating_sub(snapshot.syncing_since) > MAX_SYNCING_SECS
        {
            snapshot.syncing = false;
            snapshot.last_error = Some("sync worker exceeded its pass deadline".to_string());
        }
        snapshot
    }

    /// Register the remote-change callback (replaces any previous one).
    pub fn set_on_remote(&self, cb: RemoteCallback) {
        self.handler.set_on_remote(cb);
    }

    /// Register the pairing-event callback (replaces any previous one).
    pub fn set_pair_callback(&self, cb: PairCallback) {
        self.pair.set_on_pair(cb);
    }

    /// Accept or reject an [`PairEvent::Incoming`] request.
    pub fn respond_pair(&self, id: &str, accept: bool) {
        self.pair.respond(id, accept);
    }

    /// Take (and clear) a pending "a peer removed us from its sync" notice, if
    /// any. The app surfaces this so a mutual removal is never silent.
    pub fn take_removed_notice(&self) -> Option<String> {
        self.handler.take_removed_notice()
    }

    /// This device's outstanding invites (for the Settings list).
    pub fn invites(&self) -> Vec<Invite> {
        let now = now_secs();
        self.pair
            .invites
            .lock()
            .map(|mut list| {
                list.retain(|i| i.expires_at > now);
                list.clone()
            })
            .unwrap_or_default()
    }

    /// Create a new invite and return its shareable ticket. Prunes expired
    /// invites and caps the outstanding count.
    pub fn create_invite(&self) -> Result<String> {
        let secret = ticket::generate_secret();
        let now = now_secs();
        let invite = Invite {
            id: hex_encode(&secret[..4]),
            secret: hex_encode(&secret),
            created_at: now,
            expires_at: now + pair::INVITE_TTL_SECS,
        };
        {
            let mut invites = self.pair.invites.lock().unwrap_or_else(|e| e.into_inner());
            invites.retain(|i| i.expires_at > now);
            while invites.len() >= pair::MAX_INVITES {
                invites.remove(0);
            }
            invites.push(invite);
            pair::save_invites(&invites);
        }
        Ok(ticket::encode_ticket(&self.endpoint.id(), &secret))
    }

    /// Cancel (revoke) an outstanding invite by its short id.
    pub fn cancel_invite(&self, id: &str) {
        if let Ok(mut invites) = self.pair.invites.lock() {
            invites.retain(|i| i.id != id);
            pair::save_invites(&invites);
        }
    }

    /// Join another device using its invite ticket. The ticket is parsed
    /// immediately (bad input returns an error) and the connection attempt is
    /// retried in the background until it succeeds or expires.
    pub fn join_invite(&self, ticket_text: &str) -> Result<()> {
        ticket::decode_ticket(ticket_text)?;
        pair::save_pending_join(ticket_text.trim());
        self.spawn_join();
        Ok(())
    }

    fn spawn_join(&self) {
        let endpoint = self.endpoint.clone();
        let handler = self.handler.clone();
        let status = self.status.clone();
        let on_pair = self.pair.on_pair.clone();
        let sync_notify = self.sync_notify.clone();
        self.rt.spawn(async move {
            attempt_join(endpoint, handler, status, on_pair, sync_notify).await;
        });
    }

    /// Ask the sync worker to run a pass. Coalesced: a request made while a
    /// pass is running queues exactly one follow-up pass.
    pub fn sync_now(&self) {
        if std::env::var_os("NOVA_SYNC_DEBUG").is_some() {
            eprintln!("[sync] trigger=explicit");
        }
        self.force_attempt.store(true, Ordering::Release);
        self.sync_notify.notify_one();
    }

    /// Tell iroh the device's connectivity may have changed. Android does not
    /// notify native code of network changes, so the app polls and forwards
    /// them here; this re-establishes relay/direct paths, then requests a sync
    /// so a regained network is used promptly.
    pub fn notify_network_change(&self) {
        if self.network_refreshing.swap(true, Ordering::AcqRel) {
            return;
        }
        if std::env::var_os("NOVA_SYNC_DEBUG").is_some() {
            eprintln!("[sync] trigger=network_refresh");
        }
        // Drop cached connections: their paths/relay assignment may be stale
        // after a network change, so redial rather than reuse.
        self.conns.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let endpoint = self.endpoint.clone();
        let refreshing = self.network_refreshing.clone();
        let force_attempt = self.force_attempt.clone();
        let notify = self.sync_notify.clone();
        let stop = self.stop.clone();
        self.rt.spawn(async move {
            recover_network(
                endpoint.network_change(),
                &refreshing,
                &force_attempt,
                &notify,
                &stop,
            )
            .await;
        });
    }

    /// Record a local change. `ts` is the observation time (0 = stamp now).
    /// A value of `None` is a tombstone (deletion). No-op when unchanged.
    pub fn notify(&self, domain: &str, key: &str, value: Option<&str>, ts: u64) {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        if store.set(domain, key, value.map(str::to_string), ts, self.device) {
            if let Err(e) = store.save() {
                nova_storage::report(nova_storage::Error::new(
                    nova_storage::ErrorKind::Transaction,
                    e,
                ));
            }
        }
    }

    /// Record a batch of local changes for one domain with a single store
    /// save (one redb transaction for all rows). Each entry is
    /// `(key, value, observed_secs)`; `None` is a tombstone. Used by the app's
    /// snapshot diffing, where notifying one record at a time would otherwise
    /// cost a save per record.
    pub fn notify_batch(&self, domain: &str, changes: &[(String, Option<String>, u64)]) {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = false;
        for (key, value, ts) in changes {
            changed |= store.set(domain, key, value.clone(), *ts, self.device);
        }
        if changed {
            if let Err(e) = store.save() {
                nova_storage::report(nova_storage::Error::new(
                    nova_storage::ErrorKind::Transaction,
                    e,
                ));
            }
        }
    }

    /// Live values for a domain (tombstones omitted), for the app to
    /// materialize its own types from.
    pub fn records(&self, domain: &str) -> Vec<(String, String)> {
        self.store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records(domain)
    }
    pub fn record(&self, domain: &str, key: &str) -> Option<Record> {
        self.store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record(domain, key)
            .cloned()
    }

    /// Domains that currently hold at least one record (live or tombstone).
    pub fn domains(&self) -> Vec<String> {
        self.store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .domains()
    }

    /// Stop the router and background loops. The engine must not be reused.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.sync_notify.notify_one();
        self.cadence_notify.notify_one();
        self.conns.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let router = self.router.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(router) = router {
            self.rt.spawn(async move {
                let _ = router.shutdown().await;
            });
        }
    }

    fn refresh_peer_count(&self) {
        let count = self.peers.lock().map(|p| p.len()).unwrap_or(0);
        if let Ok(mut status) = self.status.lock() {
            status.peer_count = count;
        }
    }
}

/// Long-lived sync worker: one bounded-parallel pass per wake-up. A trigger
/// that arrives during a pass leaves a `Notify` permit, so it runs exactly one
/// more pass afterwards (coalesced) instead of being dropped.
async fn sync_worker(
    endpoint: Endpoint,
    handler: Arc<Handler>,
    peers: Arc<Mutex<Vec<String>>>,
    conns: Arc<Mutex<HashMap<String, Connection>>>,
    health: Arc<Mutex<HashMap<String, PeerHealth>>>,
    sync_notify: Arc<tokio::sync::Notify>,
    stop: Arc<AtomicBool>,
    force_attempt: Arc<AtomicBool>,
) {
    loop {
        sync_notify.notified().await;
        if stop.load(Ordering::Relaxed) {
            return;
        }
        // Explicit requests and refreshed connectivity get one bounded retry.
        // Periodic/pairing wakes retain normal backoff. A request during a pass
        // is consumed only by its coalesced follow-up, not the current pass.
        if force_attempt.swap(false, Ordering::AcqRel) {
            health.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
        // Bound the whole pass. If it overruns, drop it (aborting its spawned
        // tasks) and clear the flag so the UI recovers; the next pass retries.
        match tokio::time::timeout(
            Duration::from_secs(MAX_PASS_SECS),
            run_pass(
                endpoint.clone(),
                handler.clone(),
                peers.clone(),
                conns.clone(),
                health.clone(),
            ),
        )
        .await
        {
            Ok(_) => {}
            Err(_) => {
                eprintln!("nova sync: pass timed out after {MAX_PASS_SECS}s; aborting");
                handler.finish_sync(Some(format!("sync pass timed out after {MAX_PASS_SECS}s")));
            }
        }
        // Count worker wakes even with no eligible peer. Android uses the
        // separate one-shot outcome, never this counter, for completion.
        handler.note_pass_done();
    }
}

/// Run one sync pass over all peers not in backoff, up to [`SYNC_CONCURRENCY`]
/// at a time.
///
/// A pass can discover peers it did not know about (a peer shares its `peers`
/// domain, introducing the rest of the mesh). Rather than wait for the next
/// interval, we immediately run another round against the grown set, so a newly
/// paired device fans out across the whole mesh in one wake-up. Bounded by
/// [`MAX_PASS_ROUNDS`] so cluster-wide convergence cannot loop forever.
async fn run_pass(
    endpoint: Endpoint,
    handler: Arc<Handler>,
    peers: Arc<Mutex<Vec<String>>>,
    conns: Arc<Mutex<HashMap<String, Connection>>>,
    health: Arc<Mutex<HashMap<String, PeerHealth>>>,
) -> OneShotOutcome {
    let debug = std::env::var_os("NOVA_SYNC_DEBUG").is_some();
    let started_at = now_secs();
    let mut last_err = None;
    let mut started = false;
    for _ in 0..MAX_PASS_ROUNDS {
        let before: HashSet<String> = peers
            .lock()
            .map(|p| p.iter().cloned().collect())
            .unwrap_or_default();
        let now = Instant::now();
        let targets: Vec<String> = before
            .iter()
            .filter(|id| !should_skip(&health, id, now))
            .cloned()
            .collect();
        if targets.is_empty() {
            break;
        }
        if !started {
            handler.set_syncing(true);
            started = true;
        }
        let semaphore = Arc::new(tokio::sync::Semaphore::new(SYNC_CONCURRENCY));
        let mut tasks = tokio::task::JoinSet::new();
        for peer in targets {
            let endpoint = endpoint.clone();
            let handler = handler.clone();
            let conns = conns.clone();
            let health = health.clone();
            let semaphore = semaphore.clone();
            tasks.spawn(async move {
                let _permit = semaphore.acquire_owned().await;
                sync_one(&endpoint, &handler, &peer, &conns, &health).await
            });
        }
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Some(err)) => last_err = Some(err),
                Ok(None) => {}
                Err(e) => last_err = Some(format!("sync task failed: {e}")),
            }
        }
        // Did this round introduce a peer we had not dialed? If so, go again.
        let after: HashSet<String> = peers
            .lock()
            .map(|p| p.iter().cloned().collect())
            .unwrap_or_default();
        if after.difference(&before).next().is_none() {
            break;
        }
    }
    if started {
        handler.finish_sync(last_err.clone());
    }
    if debug {
        eprintln!(
            "[sync] pass done in {}s (started={started})",
            now_secs().saturating_sub(started_at)
        );
    }
    // Reclaim tombstones that every current peer has now acknowledged. Runs
    // after the round loop so acks recorded by this pass are taken into
    // account immediately.
    handler.gc_tombstones(now_ms());
    if let Some(error) = last_err {
        OneShotOutcome::Failed(error)
    } else if started {
        OneShotOutcome::Completed
    } else {
        OneShotOutcome::Skipped
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum OneShotOutcome {
    Completed,
    Cancelled,
    TimedOut,
    Skipped,
    Failed(String),
}

/// Sync with one peer, recording success/failure for backoff. Returns the
/// error string on failure.
async fn sync_one(
    endpoint: &Endpoint,
    handler: &Arc<Handler>,
    peer: &str,
    conns: &Arc<Mutex<HashMap<String, Connection>>>,
    health: &Arc<Mutex<HashMap<String, PeerHealth>>>,
) -> Option<String> {
    let started = Instant::now();
    handler.note_peer_attempt(peer);
    let id = match EndpointId::from_str(peer) {
        Ok(id) => id,
        Err(e) => {
            record_failure(health, peer);
            let msg = format!("bad peer id {peer}: {e}");
            handler.note_peer_result(
                peer,
                Some(msg.clone()),
                BACKOFF_BASE_SECS,
                started.elapsed(),
            );
            return Some(msg);
        }
    };
    let conn = match peer_connection(endpoint, conns, peer, id).await {
        Ok(conn) => conn,
        Err(msg) => {
            record_failure(health, peer);
            let failures = health
                .lock()
                .unwrap()
                .get(peer)
                .map(|h| h.fail_count)
                .unwrap_or(1);
            let retry = (BACKOFF_BASE_SECS * (1u64 << failures.saturating_sub(1).min(6)))
                .min(BACKOFF_MAX_SECS);
            handler.note_peer_result(peer, Some(msg.clone()), retry, started.elapsed());
            return Some(msg);
        }
    };
    let outcome = match tokio::time::timeout(
        EXCHANGE_TIMEOUT,
        protocol::run(conn.clone(), true, handler),
    )
    .await
    {
        Ok(Ok(())) => {
            record_success(health, peer);
            None
        }
        Ok(Err(e)) => {
            // A cached connection may have gone stale between passes; drop it
            // so the next pass redials instead of failing against it again.
            forget_connection(conns, peer);
            let msg = format!("{e:#}");
            // The remote explicitly refused us as a peer: it removed us. Drop
            // it instead of retrying (and backing off) forever.
            if matches!(conn.close_reason(), Some(iroh::endpoint::ConnectionError::ApplicationClosed(ref reason)) if reason.error_code == protocol::REVOKED_CODE.into())
            {
                handler.note_rejected(peer);
            }
            record_failure(health, peer);
            Some(msg)
        }
        Err(_) => {
            forget_connection(conns, peer);
            record_failure(health, peer);
            Some(format!("sync with {peer} timed out"))
        }
    };
    let failures = health
        .lock()
        .unwrap()
        .get(peer)
        .map(|h| h.fail_count)
        .unwrap_or(0);
    let retry = if failures == 0 {
        0
    } else {
        (BACKOFF_BASE_SECS * (1u64 << failures.saturating_sub(1).min(6))).min(BACKOFF_MAX_SECS)
    };
    handler.note_peer_result(peer, outcome.clone(), retry, started.elapsed());
    if std::env::var_os("NOVA_SYNC_DEBUG").is_some() {
        eprintln!(
            "[sync] peer {}: {}",
            &peer[..peer.len().min(12)],
            outcome.as_deref().unwrap_or("ok")
        );
    }
    outcome
}

/// Return the cached connection to `peer`, or dial a fresh one. A cached
/// connection that has already closed is discarded and redialed. The dialer
/// keeps one connection per peer and opens a fresh bi stream per pass, so the
/// QUIC handshake is paid once instead of every pass.
async fn peer_connection(
    endpoint: &Endpoint,
    conns: &Arc<Mutex<HashMap<String, Connection>>>,
    peer: &str,
    id: EndpointId,
) -> Result<Connection, String> {
    // The if-let scrutinee's temporary guard otherwise lives through the
    // body, deadlocking when eviction tries to lock the same cache again.
    let cached = {
        conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(peer)
            .cloned()
    };
    if let Some(conn) = cached {
        if conn.close_reason().is_none() {
            if std::env::var_os("NOVA_SYNC_DEBUG").is_some() {
                eprintln!("[sync] connection=reuse peer={}", short_id(peer));
            }
            return Ok(conn);
        }
        forget_connection(conns, peer);
    }
    if std::env::var_os("NOVA_SYNC_DEBUG").is_some() {
        eprintln!("[sync] connection=redial peer={}", short_id(peer));
    }
    match tokio::time::timeout(
        CONNECT_TIMEOUT,
        endpoint.connect(EndpointAddr::from(id), ALPN),
    )
    .await
    {
        Ok(Ok(conn)) => {
            conns
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(peer.to_string(), conn.clone());
            Ok(conn)
        }
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!("connect to {peer} timed out")),
    }
}

/// Drop a cached connection for `peer` after a failure, so the next pass
/// redials instead of reusing a dead handle.
fn forget_connection(conns: &Arc<Mutex<HashMap<String, Connection>>>, peer: &str) {
    conns.lock().unwrap_or_else(|e| e.into_inner()).remove(peer);
}

async fn interval_loop(
    endpoint: Endpoint,
    handler: Arc<Handler>,
    status: Arc<Mutex<SyncStatus>>,
    on_pair: Arc<Mutex<Option<PairCallback>>>,
    sync_notify: Arc<tokio::sync::Notify>,
    stop: Arc<AtomicBool>,
    cadence_notify: Arc<tokio::sync::Notify>,
) {
    let mut last_pass = tokio::time::Instant::now();
    loop {
        if !wait_for_cadence(last_pass, &cadence_notify, &stop, || {
            effective_interval(
                read_settings().interval_secs,
                FOREGROUND.load(Ordering::Relaxed),
            )
        })
        .await
        {
            return;
        }
        #[cfg(target_os = "android")]
        if !FOREGROUND_OWNER.load(Ordering::Acquire) {
            last_pass = tokio::time::Instant::now();
            continue; // headless leases run only their cancellable one-shot
        }
        // Retry an outstanding invite join before the periodic pass.
        if pair::load_pending_join().is_some() {
            attempt_join(
                endpoint.clone(),
                handler.clone(),
                status.clone(),
                on_pair.clone(),
                sync_notify.clone(),
            )
            .await;
        }
        if std::env::var_os("NOVA_SYNC_DEBUG").is_some() {
            eprintln!("[sync] trigger=periodic");
        }
        sync_notify.notify_one();
        last_pass = tokio::time::Instant::now();
    }
}

fn effective_interval(interval: u64, foreground: bool) -> u64 {
    (if foreground {
        interval
    } else {
        interval.max(300)
    })
    .clamp(5, 24 * 60 * 60)
}

/// Recompute from the last periodic wake, not from the settings-change time.
/// Reducing an already-elapsed interval therefore wakes promptly; increasing
/// it postpones the old deadline rather than issuing one extra early pass.
async fn wait_for_cadence(
    last_pass: tokio::time::Instant,
    notify: &tokio::sync::Notify,
    stop: &AtomicBool,
    interval: impl Fn() -> u64,
) -> bool {
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        tokio::select! {
            biased;
            _ = notify.notified() => continue,
            _ = tokio::time::sleep_until(last_pass + Duration::from_secs(interval())) => {
                return !stop.load(Ordering::Relaxed);
            }
        }
    }
}

async fn recover_network(
    refresh: impl std::future::Future<Output = ()>,
    refreshing: &AtomicBool,
    force_attempt: &AtomicBool,
    notify: &tokio::sync::Notify,
    stop: &AtomicBool,
) {
    refresh.await;
    refreshing.store(false, Ordering::Release);
    if !stop.load(Ordering::Relaxed) {
        force_attempt.store(true, Ordering::Release);
        notify.notify_one();
    }
}

/// Try to complete an outstanding join (from `join_invite` or the periodic
/// retry). On success the host is added to the peer list and a sync is
/// requested; on failure the pending join is kept for the next attempt.
async fn attempt_join(
    endpoint: Endpoint,
    handler: Arc<Handler>,
    status: Arc<Mutex<SyncStatus>>,
    on_pair: Arc<Mutex<Option<PairCallback>>>,
    sync_notify: Arc<tokio::sync::Notify>,
) {
    let Ok(_join) = handler.joining.try_lock() else {
        return;
    };
    let Some(pending) = pair::load_pending_join() else {
        return;
    };
    if pending.expires_at <= now_secs() {
        pair::clear_pending_join();
        return;
    }
    let (host_id, secret) = match ticket::decode_ticket(&pending.ticket) {
        Ok(value) => value,
        Err(e) => {
            pair::clear_pending_join();
            if let Ok(mut s) = status.lock() {
                s.last_error = Some(format!("invalid invite: {e}"));
            }
            return;
        }
    };
    let name = effective_device_name(&read_settings());
    let host_id_string = host_id.to_string();
    match tokio::time::timeout(
        Duration::from_secs(90),
        pair::initiate_with_trust(
            &endpoint,
            EndpointAddr::from(host_id),
            &secret,
            &name,
            &host_id_string,
            |name| handler.add_peer_explicit(&host_id_string, name),
        ),
    )
    .await
    .unwrap_or_else(|_| Err(anyhow::anyhow!("pairing timed out")))
    {
        Ok(host_name) => {
            // Trust the host and record its name; an explicit re-pair is
            // allowed to re-add a peer that was previously removed.
            pair::clear_pending_join();
            if let Ok(mut s) = status.lock() {
                s.last_error = None;
                s.last_sync_secs = now_secs();
            }
            if let Some(cb) = on_pair.lock().ok().and_then(|c| c.clone()) {
                cb(PairEvent::Paired { name: host_name });
            }
            sync_notify.notify_one();
        }
        Err(e) => {
            // Keep the pending join; the interval retries it.
            if let Ok(mut s) = status.lock() {
                s.last_error = Some(format!("pairing: {e}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Process-wide singleton (mirrors `nova-torrent`)
// ---------------------------------------------------------------------------

static ENGINE: LazyLock<Mutex<Option<Arc<SyncEngine>>>> = LazyLock::new(|| Mutex::new(None));
// Setup/install/release is serialized independently of short singleton reads.
static LIFECYCLE: Mutex<()> = Mutex::new(());
static FOREGROUND_OWNER: AtomicBool = AtomicBool::new(false);

pub fn foreground_engine() -> Result<Arc<SyncEngine>> {
    let _transition = LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
    if engine().is_none() {
        install_inner(SyncEngine::setup()?);
    }
    FOREGROUND_OWNER.store(true, Ordering::Release);
    engine().context("engine unavailable after setup")
}

pub struct BackgroundLease {
    pub engine: Arc<SyncEngine>,
}
impl Drop for BackgroundLease {
    fn drop(&mut self) {
        let _transition = LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
        let mut slot = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        if !FOREGROUND_OWNER.load(Ordering::Acquire)
            && slot.as_ref().is_some_and(|e| Arc::ptr_eq(e, &self.engine))
        {
            if let Some(engine) = slot.take() {
                engine.stop();
            }
        }
    }
}
pub fn background_engine() -> Result<Option<BackgroundLease>> {
    let _transition = LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
    if engine().is_some() {
        return Ok(None);
    }
    install_inner(SyncEngine::setup()?);
    Ok(engine().map(|engine| BackgroundLease { engine }))
}

pub fn install(engine: SyncEngine) {
    let _transition = LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
    FOREGROUND_OWNER.store(true, Ordering::Release);
    install_inner(engine);
}
fn install_inner(engine: SyncEngine) {
    let mut slot = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = slot.replace(Arc::new(engine)) {
        old.stop();
    }
}

pub fn uninstall() {
    let _transition = LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
    FOREGROUND_OWNER.store(false, Ordering::Release);
    let old = ENGINE.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(old) = old {
        old.stop();
    }
}

pub fn engine() -> Option<Arc<SyncEngine>> {
    ENGINE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Forward a connectivity change to the running engine (no-op when sync is
/// off). Called by the app's Android network poll.
pub fn notify_network_change() {
    if let Some(engine) = engine() {
        engine.notify_network_change();
    }
}

pub fn is_running() -> bool {
    ENGINE.lock().map(|e| e.is_some()).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Identity + peer persistence helpers
// ---------------------------------------------------------------------------

fn load_or_create_secret() -> Result<SecretKey> {
    static IDENTITY_LOCK: Mutex<()> = Mutex::new(());
    let _identity = IDENTITY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(hex) = nova_storage::try_get_str(IDENTITY_KEY)? {
        let bytes = hex_decode(&hex)
            .context("unreadable sync identity; restore backup, do not regenerate")?;
        let arr = <[u8; 32]>::try_from(bytes.as_slice()).context("invalid sync identity length")?;
        return Ok(SecretKey::from_bytes(&arr));
    }
    let secret = SecretKey::generate();
    nova_storage::try_set_str(IDENTITY_KEY, &hex_encode(&secret.to_bytes()))?;
    Ok(secret)
}

/// The `peers` sync domain: key = endpoint id, value = JSON [`PeerValue`].
/// Peer ids and names replicate across the mesh, so pairing one device makes
/// it known to the others.
pub const DOMAIN_PEERS: &str = "peers";

/// The `presence` sync domain: key = `{observer}\x01{peer}`, value = unix
/// seconds of the observer's last completed exchange with the peer. Each
/// device only writes its own observer keys, so records never conflict
/// across writers; readers take the max across observers. This is how a
/// device learns that a peer it hasn't dialed recently was seen alive by
/// someone else in the mesh.
pub const DOMAIN_PRESENCE: &str = "presence";

/// Minimum gap between our own presence rewrites for one peer. Without it,
/// steady syncing would dirty the store (and the next digest) on every pass.
const PRESENCE_WRITE_MIN_SECS: u64 = 15 * 60;

/// Composite presence key for `observer`'s view of `peer`.
fn presence_key(observer: &str, peer: &str) -> String {
    format!("{observer}\x01{peer}")
}

/// Parse a presence value (unix secs); garbage never blocks the max.
fn parse_presence(value: &str) -> Option<u64> {
    serde_json::from_str(value.trim_matches('"')).ok()
}

/// Record that `peer` completed an exchange with us just now, publishing our
/// observation into the mesh `presence` domain. Throttled: a still-fresh
/// observation is left alone. Never records our own id.
pub(crate) fn presence_note(store: &Arc<Mutex<Store>>, self_id: &str, peer: &str, dev: u64) {
    if peer == self_id {
        return;
    }
    let now = now_secs();
    let key = presence_key(self_id, peer);
    let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(rec) = store.record(DOMAIN_PRESENCE, &key)
        && !rec.is_deleted()
        && let Some(prev) = rec.value.as_deref().and_then(parse_presence)
        && now.saturating_sub(prev) < PRESENCE_WRITE_MIN_SECS
    {
        return;
    }
    if store.set(DOMAIN_PRESENCE, &key, Some(now.to_string()), 0, dev) {
        if let Err(e) = store.save() {
            nova_storage::report(nova_storage::Error::new(
                nova_storage::ErrorKind::Transaction,
                e,
            ));
        }
    }
}

/// Freshest mesh observation of `peer` (unix secs), across all observers.
pub(crate) fn presence_newest(store: &Arc<Mutex<Store>>, peer: &str) -> Option<u64> {
    let store = store.lock().unwrap_or_else(|e| e.into_inner());
    store
        .records(DOMAIN_PRESENCE)
        .into_iter()
        .filter_map(|(key, value)| {
            let (_, subject) = key.split_once('\x01')?;
            (subject == peer).then(|| parse_presence(&value))?
        })
        .max()
}

/// Per-peer acknowledgement clocks: endpoint id -> the HLC snapshot of our
/// store at the last completed sync with that peer. Local only; see
/// [`Store::gc`] for how it gates tombstone collection.
pub(crate) type PeerAcks = HashMap<String, Hlc>;

/// One persisted ack entry. Older builds stored a bare unix-seconds number;
/// accept both so an upgrade doesn't lose the acks.
#[derive(Deserialize)]
#[serde(untagged)]
enum AckEntry {
    Hlc(Hlc),
    Secs(u64),
}

impl AckEntry {
    fn into_hlc(self) -> Hlc {
        match self {
            AckEntry::Hlc(hlc) => hlc,
            AckEntry::Secs(secs) => Hlc::new(secs.saturating_mul(1000), 0),
        }
    }
}

/// Load the local per-peer ack map (empty when absent/unreadable).
pub(crate) fn load_peer_acks() -> PeerAcks {
    let raw: HashMap<String, AckEntry> = nova_storage::get_str(PEER_ACKS_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    raw.into_iter().map(|(k, v)| (k, v.into_hlc())).collect()
}

/// The tombstone GC floor from the current peers' ack clocks. With no peers,
/// plain TTL expiry applies; if any peer has never acked, retain everything.
pub(crate) fn ack_floor(peers: &[String], acks: &PeerAcks) -> AckFloor {
    if peers.is_empty() {
        return AckFloor::NoPeers;
    }
    let mut floor: Option<Hlc> = None;
    for id in peers {
        let Some(ack) = acks.get(id).copied() else {
            return AckFloor::Blocked;
        };
        floor = Some(match floor {
            Some(current) if !ack.newer_than(current) => current,
            _ => ack,
        });
    }
    match floor {
        Some(hlc) => AckFloor::At(hlc),
        None => AckFloor::NoPeers,
    }
}

/// Value stored for one peer. `name` is empty for peers added manually;
/// `added_at` is the time the peer entered the mesh (synced, so every device
/// orders the list identically).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PeerValue {
    #[serde(default)]
    name: String,
    #[serde(default)]
    added_at: u64,
}

/// Live value of a peer record (tombstones omitted).
fn read_peer_value(store: &Store, id: &str) -> Option<PeerValue> {
    store
        .records(DOMAIN_PEERS)
        .into_iter()
        .find(|(key, _)| key == id)
        .and_then(|(_, value)| serde_json::from_str(&value).ok())
}

/// Insert or update a peer record. `insert` allows creating an absent record;
/// `resurrect` allows overwriting an existing tombstone. Explicit pairing sets
/// both; a routine sync sets neither, so a removed peer that merely reconnects
/// is never silently re-added. Returns true when the store changed.
fn peer_upsert(
    store: &mut Store,
    id: &str,
    name: Option<&str>,
    ts: u64,
    dev: u64,
    insert: bool,
    resurrect: bool,
) -> bool {
    let existing = read_peer_value(store, id);
    let deleted = matches!(store.record(DOMAIN_PEERS, id), Some(r) if r.is_deleted());
    if existing.is_none() {
        if deleted && !resurrect {
            return false;
        }
        if !deleted && !insert {
            return false;
        }
    }
    // An empty or self-referential name carries no label; it must never clear a
    // name already learned for a live peer.
    let name = name.map(str::trim).filter(|n| !n.is_empty() && *n != id);
    if existing.is_some() && name.is_none() {
        return false;
    }
    let added_at = existing
        .as_ref()
        .map(|p| p.added_at)
        .filter(|a| *a != 0)
        .unwrap_or(ts);
    let name = name.unwrap_or("");
    if existing
        .as_ref()
        .is_some_and(|p| p.name == name && p.added_at == added_at)
    {
        return false;
    }
    let value = serde_json::to_string(&PeerValue {
        name: name.to_string(),
        added_at,
    })
    .unwrap_or_else(|_| "{}".to_string());
    store.set(DOMAIN_PEERS, id, Some(value), ts, dev)
}

/// Peer ids in mesh order: most recently added first, id as a tiebreak. Both
/// the adder and devices that learn the peer via sync compute the same order
/// from the synced `added_at`.
fn ordered_peer_ids(store: &Store, self_id: &str) -> Vec<String> {
    let mut entries: Vec<(String, u64)> = store
        .records(DOMAIN_PEERS)
        .into_iter()
        .filter(|(id, _)| id != self_id)
        .map(|(id, value)| {
            let added_at = serde_json::from_str::<PeerValue>(&value)
                .map(|p| p.added_at)
                .unwrap_or(0);
            (id, added_at)
        })
        .collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    entries.into_iter().map(|(id, _)| id).collect()
}

/// Short form of a peer id for logs.
fn short_id(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

/// Add (or refresh) a peer in both the store and the live allowlist. Used for
/// explicit user actions (pairing, manual add), which are allowed to resurrect
/// a previously removed peer. The store and the live list are updated under the
/// store lock so a concurrent reconcile cannot clobber the update.
pub(crate) fn peers_add(
    store: &Arc<Mutex<Store>>,
    peers: &Arc<Mutex<Vec<String>>>,
    id: &str,
    name: Option<&str>,
    dev: u64,
) -> Result<()> {
    let ts = now_secs();
    let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
    let changed = peer_upsert(&mut store, id, name, ts, dev, true, true);
    if changed {
        store.save()?;
        if std::env::var_os("NOVA_SYNC_DEBUG").is_some() {
            eprintln!("[sync] peer added/updated {}", short_id(id));
        }
    }
    let mut list = peers.lock().unwrap_or_else(|e| e.into_inner());
    if !list.iter().any(|p| p == id) {
        list.push(id.to_string());
    }
    Ok(())
}

/// Refresh the name of an already-live peer during a sync. Never inserts and
/// never resurrects a tombstone: only an explicit re-pair may re-add a peer.
pub(crate) fn peers_refresh(
    store: &Arc<Mutex<Store>>,
    peers: &Arc<Mutex<Vec<String>>>,
    id: &str,
    name: &str,
    dev: u64,
) {
    let ts = now_secs();
    let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
    if peer_upsert(&mut store, id, Some(name), ts, dev, false, false) {
        if let Err(e) = store.save() {
            nova_storage::report(nova_storage::Error::new(
                nova_storage::ErrorKind::Transaction,
                e,
            ));
        }
    }
    // A peer we are syncing with is live by construction; make sure the
    // allowlist agrees (defensive against a missed reconcile).
    let live = read_peer_value(&store, id).is_some();
    let mut list = peers.lock().unwrap_or_else(|e| e.into_inner());
    if live && !list.iter().any(|p| p == id) {
        list.push(id.to_string());
    }
}

/// Remove a peer everywhere (tombstone propagates across the mesh).
pub(crate) fn peers_remove(
    store: &Arc<Mutex<Store>>,
    peers: &Arc<Mutex<Vec<String>>>,
    id: &str,
    dev: u64,
) {
    let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
    let now = now_secs();
    let mut changed = store.set(DOMAIN_PEERS, id, None, now, dev);
    // Drop mesh presence observations involving the removed peer (as
    // observer or subject), so orphan sightings don't linger: the
    // tombstones replicate like any other removal.
    let stale: Vec<String> = store
        .records(DOMAIN_PRESENCE)
        .into_iter()
        .filter_map(|(key, _)| {
            let (observer, subject) = key.split_once('\x01')?;
            (observer == id || subject == id).then(|| key.clone())
        })
        .collect();
    for key in stale {
        changed |= store.set(DOMAIN_PRESENCE, &key, None, now, dev);
    }
    if changed {
        if let Err(e) = store.save() {
            nova_storage::report(nova_storage::Error::new(
                nova_storage::ErrorKind::Transaction,
                e,
            ));
        }
        eprintln!("nova sync: removed peer {}", short_id(id));
    }
    let mut list = peers.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|p| p != id);
}

/// Rebuild the live allowlist from the merged `peers` domain after a sync,
/// excluding our own id. Returns true when the allowlist changed. The store is
/// read and the list replaced while holding the store lock, so this cannot
/// interleave with `peers_add`/`peers_remove` and lose an update.
pub(crate) fn reconcile_peers(
    store: &Arc<Mutex<Store>>,
    peers: &Arc<Mutex<Vec<String>>>,
    self_id: &str,
) -> bool {
    let store = store.lock().unwrap_or_else(|e| e.into_inner());
    let live = ordered_peer_ids(&store, self_id);
    let mut list = peers.lock().unwrap_or_else(|e| e.into_inner());
    if *list == live {
        return false;
    }
    *list = live;
    true
}

/// Read the friendly names for all known peers.
pub(crate) fn peer_names_from_store(
    store: &Arc<Mutex<Store>>,
) -> std::collections::HashMap<String, String> {
    let store = store.lock().unwrap_or_else(|e| e.into_inner());
    store
        .records(DOMAIN_PEERS)
        .into_iter()
        .filter_map(|(id, value)| {
            let parsed: PeerValue = serde_json::from_str(&value).ok()?;
            (!parsed.name.trim().is_empty()).then_some((id, parsed.name))
        })
        .collect()
}

/// Legacy peer list + names (pre-mesh), used only for the one-time migration.
fn load_legacy_peers() -> Vec<(String, String)> {
    let ids: Vec<String> = nova_storage::get_str(PEERS_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let names: std::collections::HashMap<String, String> = nova_storage::get_str(PEER_NAMES_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    ids.into_iter()
        .map(|id| {
            let name = names.get(&id).cloned().unwrap_or_default();
            (id, name)
        })
        .collect()
}

/// Seed the `peers` domain from the legacy list (idempotent). Never resurrects
/// a peer that was later removed. Returns true when anything was written.
fn migrate_peers(store: &mut Store, legacy: &[(String, String)], ts: u64, dev: u64) -> bool {
    let mut changed = false;
    for (id, name) in legacy {
        let name = (!name.is_empty()).then_some(name.as_str());
        changed |= peer_upsert(store, id, name, ts, dev, true, false);
    }
    changed
}

fn allowlist_from_store(store: &Store, self_id: &str) -> Vec<String> {
    ordered_peer_ids(store, self_id)
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

pub(crate) fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for pair in bytes.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_new_fields_default_to_current_behavior() {
        // Settings persisted before this field existed must parse with
        // the behavior-preserving default: discovery on.
        let old: SyncSettings = serde_json::from_str(
            r#"{"enabled":true,"interval_secs":300,"device_name":"x","require_confirmation":true}"#,
        )
        .unwrap();
        assert!(old.enable_local_discovery);
        // `background_enabled` is likewise new: default on, local-only.
        assert!(old.background_enabled);
        assert_eq!(SyncSettings::default().interval_secs, 60);
        // Round-trip keeps explicit values.
        let new = SyncSettings {
            enable_local_discovery: false,
            ..Default::default()
        };
        let back: SyncSettings =
            serde_json::from_str(&serde_json::to_string(&new).unwrap()).unwrap();
        assert_eq!(back, new);
    }

    #[test]
    fn presence_note_writes_and_throttles() {
        let store = Arc::new(Mutex::new(Store::default()));
        presence_note(&store, "self", "peer-a", 1);
        let first: u64 = serde_json::from_str(
            &store
                .lock()
                .unwrap()
                .records(DOMAIN_PRESENCE)
                .into_iter()
                .find(|(k, _)| k == &presence_key("self", "peer-a"))
                .map(|(_, v)| v)
                .expect("presence record"),
        )
        .unwrap();
        assert!(first > 0);
        // A second sighting right away leaves the record value alone
        // (throttled: no rewrite, no clock tick).
        presence_note(&store, "self", "peer-a", 1);
        let second: u64 = serde_json::from_str(
            &store
                .lock()
                .unwrap()
                .records(DOMAIN_PRESENCE)
                .into_iter()
                .find(|(k, _)| k == &presence_key("self", "peer-a"))
                .map(|(_, v)| v)
                .expect("presence record"),
        )
        .unwrap();
        assert_eq!(second, first);
        // Self-sightings are never recorded.
        presence_note(&store, "self", "self", 1);
        assert!(
            store
                .lock()
                .unwrap()
                .records(DOMAIN_PRESENCE)
                .iter()
                .all(|(k, _)| !k.ends_with("self"))
        );
    }

    #[test]
    fn presence_newest_takes_max_and_ignores_garbage() {
        let store = Arc::new(Mutex::new(Store::default()));
        {
            let mut s = store.lock().unwrap();
            s.set(
                DOMAIN_PRESENCE,
                &presence_key("a", "p"),
                Some("100".into()),
                0,
                1,
            );
            s.set(
                DOMAIN_PRESENCE,
                &presence_key("b", "p"),
                Some("300".into()),
                0,
                2,
            );
            s.set(
                DOMAIN_PRESENCE,
                &presence_key("c", "p"),
                Some("not-a-time".into()),
                0,
                3,
            );
            s.set(
                DOMAIN_PRESENCE,
                &presence_key("a", "other"),
                Some("999".into()),
                0,
                1,
            );
        }
        assert_eq!(presence_newest(&store, "p"), Some(300));
        assert_eq!(presence_newest(&store, "other"), Some(999));
        assert_eq!(presence_newest(&store, "nobody"), None);
    }

    #[test]
    fn peers_remove_tombstones_presence() {
        let store = Arc::new(Mutex::new(Store::default()));
        let peers = Arc::new(Mutex::new(Vec::new()));
        peers_add(&store, &peers, "peer-a", Some("A"), 1).unwrap();
        peers_add(&store, &peers, "peer-b", Some("B"), 1).unwrap();
        presence_note(&store, "self", "peer-a", 1);
        presence_note(&store, "peer-b", "peer-a", 2);
        presence_note(&store, "self", "peer-b", 1);
        peers_remove(&store, &peers, "peer-a", 1);
        let live: Vec<String> = store
            .lock()
            .unwrap()
            .records(DOMAIN_PRESENCE)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        // Sightings involving peer-a are gone; peer-b's own sighting stays.
        assert!(!live.iter().any(|k| k.contains("peer-a")));
        assert_eq!(live, vec![presence_key("self", "peer-b")]);
    }

    #[test]
    fn hex_round_trip() {
        let bytes: Vec<u8> = (0..=255u8).collect();
        let encoded = hex_encode(&bytes);
        assert_eq!(encoded.len(), 512);
        assert_eq!(hex_decode(&encoded).unwrap(), bytes);
    }

    #[test]
    fn hex_rejects_bad_input() {
        assert!(hex_decode("abc").is_none());
        assert!(hex_decode("zz").is_none());
    }

    #[test]
    fn migrate_peers_seeds_ids_and_names() {
        let mut store = Store::default();
        let legacy = vec![
            ("peer-a".to_string(), "Phone".to_string()),
            ("peer-b".to_string(), String::new()),
        ];
        assert!(migrate_peers(&mut store, &legacy, 100, 7));
        // Idempotent.
        assert!(!migrate_peers(&mut store, &legacy, 200, 7));
        let live: Vec<String> = allowlist_from_store(&store, "");
        assert!(live.contains(&"peer-a".to_string()));
        assert!(live.contains(&"peer-b".to_string()));
    }

    #[test]
    fn peer_names_round_trip_and_ignore_placeholders() {
        let store = Arc::new(Mutex::new(Store::default()));
        let peers = Arc::new(Mutex::new(Vec::new()));
        peers_add(&store, &peers, "peer-a", Some("Pixel 8"), 1).unwrap();
        peers_add(&store, &peers, "peer-b", Some(""), 1).unwrap();
        let names = peer_names_from_store(&store);
        assert_eq!(names.get("peer-a").map(String::as_str), Some("Pixel 8"));
        assert!(!names.contains_key("peer-b"));
    }

    #[test]
    fn reconcile_peers_drops_removed_ids() {
        let store = Arc::new(Mutex::new(Store::default()));
        let peers = Arc::new(Mutex::new(Vec::new()));
        peers_add(&store, &peers, "peer-a", Some("A"), 1).unwrap();
        peers_add(&store, &peers, "peer-b", None, 1).unwrap();
        assert!(!reconcile_peers(&store, &peers, "")); // already in sync
        peers_remove(&store, &peers, "peer-a", 1);
        assert_eq!(peers.lock().unwrap().as_slice(), &["peer-b".to_string()]);
    }

    #[test]
    fn refresh_does_not_resurrect_a_removed_peer() {
        let store = Arc::new(Mutex::new(Store::default()));
        let peers = Arc::new(Mutex::new(Vec::new()));
        peers_add(&store, &peers, "peer-a", Some("A"), 1).unwrap();
        peers_remove(&store, &peers, "peer-a", 1);
        assert!(store.lock().unwrap().records(DOMAIN_PEERS).is_empty());

        // A routine sync refresh must not bring a removed peer back.
        peers_refresh(&store, &peers, "peer-a", "A", 1);
        assert!(!peers.lock().unwrap().contains(&"peer-a".to_string()));
        assert!(store.lock().unwrap().records(DOMAIN_PEERS).is_empty());

        // An explicit re-pair may.
        peers_add(&store, &peers, "peer-a", Some("A"), 1).unwrap();
        assert!(peers.lock().unwrap().contains(&"peer-a".to_string()));
        assert_eq!(store.lock().unwrap().records(DOMAIN_PEERS).len(), 1);
    }

    #[test]
    fn refresh_keeps_a_live_peer_name() {
        let store = Arc::new(Mutex::new(Store::default()));
        let peers = Arc::new(Mutex::new(Vec::new()));
        peers_add(&store, &peers, "peer-a", Some("Pixel"), 1).unwrap();
        // An empty name from a sync must not clear the learned label.
        peers_refresh(&store, &peers, "peer-a", "", 1);
        let names = peer_names_from_store(&store);
        assert_eq!(names.get("peer-a").map(String::as_str), Some("Pixel"));
        // A real name does update it.
        peers_refresh(&store, &peers, "peer-a", "Pixel 8", 1);
        let names = peer_names_from_store(&store);
        assert_eq!(names.get("peer-a").map(String::as_str), Some("Pixel 8"));
    }

    #[test]
    fn backoff_skips_then_recovers() {
        let health: Arc<Mutex<HashMap<String, PeerHealth>>> = Arc::new(Mutex::new(HashMap::new()));
        let now = Instant::now();
        assert!(!should_skip(&health, "p", now));
        record_failure(&health, "p");
        assert!(should_skip(&health, "p", Instant::now()));
        // Rewind the failure so its window has elapsed.
        health.lock().unwrap().get_mut("p").unwrap().last_fail = Some(now);
        assert!(!should_skip(
            &health,
            "p",
            now + Duration::from_secs(BACKOFF_BASE_SECS + 1),
        ));
        record_success(&health, "p");
        assert!(!should_skip(&health, "p", now));
    }

    #[test]
    fn interval_respects_foreground_and_safety_bounds() {
        assert_eq!(effective_interval(30, true), 30);
        assert_eq!(effective_interval(30, false), 300);
        assert_eq!(effective_interval(900, false), 900);
        assert_eq!(effective_interval(0, true), 5);
        assert_eq!(effective_interval(u64::MAX, true), 86400);
    }

    #[tokio::test]
    async fn cadence_change_recomputes_pending_deadline() {
        let notify = tokio::sync::Notify::new();
        let stop = AtomicBool::new(false);
        let interval = std::sync::atomic::AtomicU64::new(900);
        let last = tokio::time::Instant::now() - Duration::from_secs(60);
        let wait = wait_for_cadence(last, &notify, &stop, || interval.load(Ordering::Relaxed));
        tokio::pin!(wait);
        tokio::select! {
            biased;
            _ = &mut wait => panic!("long interval fired early"),
            _ = tokio::task::yield_now() => {}
        }
        interval.store(30, Ordering::Relaxed);
        notify.notify_one();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), &mut wait)
                .await
                .unwrap()
        );

        // A pending foreground wake must be postponed after backgrounding.
        interval.store(300, Ordering::Relaxed);
        notify.notify_one();
        let wait = wait_for_cadence(last, &notify, &stop, || interval.load(Ordering::Relaxed));
        tokio::pin!(wait);
        tokio::select! {
            biased;
            _ = &mut wait => panic!("background interval fired early"),
            _ = tokio::task::yield_now() => {}
        }
        stop.store(true, Ordering::Relaxed);
        notify.notify_one();
        assert!(!wait.await);
    }

    #[tokio::test]
    async fn network_recovery_wakes_only_after_refresh_and_not_after_stop() {
        let refreshing = AtomicBool::new(true);
        let force = AtomicBool::new(false);
        let stop = AtomicBool::new(false);
        let notify = tokio::sync::Notify::new();
        let (release, barrier) = tokio::sync::oneshot::channel();
        let recovery = recover_network(
            async {
                barrier.await.unwrap();
            },
            &refreshing,
            &force,
            &notify,
            &stop,
        );
        tokio::pin!(recovery);
        tokio::select! {
            biased;
            _ = &mut recovery => panic!("refresh completed without barrier"),
            _ = tokio::task::yield_now() => {}
        }
        assert!(refreshing.load(Ordering::Acquire));
        assert!(!force.load(Ordering::Acquire));
        release.send(()).unwrap();
        recovery.await;
        assert!(!refreshing.load(Ordering::Acquire));
        assert!(force.swap(false, Ordering::AcqRel));
        notify.notified().await;
        stop.store(true, Ordering::Relaxed);
        recover_network(async {}, &refreshing, &force, &notify, &stop).await;
        assert!(!force.load(Ordering::Acquire));
        tokio::select! {
            biased;
            _ = notify.notified() => panic!("stopped engine was woken"),
            _ = tokio::task::yield_now() => {}
        }
    }

    #[test]
    fn peer_order_is_newest_first_and_stable() {
        let store = Arc::new(Mutex::new(Store::default()));
        let peers = Arc::new(Mutex::new(Vec::new()));
        {
            let mut s = store.lock().unwrap();
            s.set(
                DOMAIN_PEERS,
                "older",
                Some("{\"added_at\":100}".into()),
                1,
                1,
            );
            s.set(
                DOMAIN_PEERS,
                "newer",
                Some("{\"added_at\":200}".into()),
                1,
                1,
            );
        }
        assert!(reconcile_peers(&store, &peers, ""));
        assert_eq!(
            peers.lock().unwrap().as_slice(),
            &["newer".to_string(), "older".to_string()]
        );
    }
}

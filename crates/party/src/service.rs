//! Blocking foreground-worker API around an independent, lazily bound endpoint.
use crate::engine::{self, HostRoom};
use crate::ticket::{self, Ticket};
use crate::wire::{self, Operation, Request, Response};
use crate::*;
use anyhow::{Context, Result, ensure};
use iroh::Endpoint;
use iroh::endpoint::Connection;
use iroh::protocol::{ProtocolHandler, Router};
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::Semaphore;

const EXCHANGE_TIMEOUT: Duration = Duration::from_millis(2_500);

#[derive(Debug)]
struct TerminalError(String);

impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TerminalError {}

fn pause_disconnected(snapshot: &mut RoomSnapshot, now: u64) {
    snapshot.timeline.position = snapshot
        .timeline
        .position_at(now as f64 + snapshot.clock_offset_ms)
        .clamp(0.0, snapshot.media.duration);
    snapshot.timeline.paused = true;
    snapshot.connected = false;
}

#[derive(Default)]
struct Shared {
    host: Mutex<Option<HostRoom>>,
    snapshot: RwLock<Option<RoomSnapshot>>,
}

impl Shared {
    fn publish(&self, snapshot: RoomSnapshot) {
        *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = Some(snapshot);
    }
}

struct Network {
    endpoint: Endpoint,
    router: Router,
}

struct Guest {
    ticket: Ticket,
    connection: Option<Connection>,
    next_sequence: u64,
    last_success: u64,
    best_rtt: f64,
    best_sample_at: u64,
    offset: f64,
}

#[derive(Default)]
struct Session {
    network: Option<Network>,
    guest: Option<Guest>,
}

/// All mutating methods block briefly and belong on application workers. The
/// application calls `report` every 250–500ms while foregrounded, and `leave`
/// when its foreground party lifecycle ends. There is no background poller.
pub struct PartyService {
    runtime: OnceLock<Runtime>,
    session: Mutex<Session>,
    shared: Arc<Shared>,
}

impl PartyService {
    pub fn new() -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            runtime: OnceLock::new(),
            session: Mutex::new(Session::default()),
            shared: Arc::new(Shared::default()),
        }))
    }

    fn runtime(&self) -> Result<&Runtime> {
        if self.runtime.get().is_none() {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .context("create party runtime")?;
            let _ = self.runtime.set(runtime);
        }
        Ok(self.runtime.get().expect("runtime initialized"))
    }

    async fn bind(&self) -> Result<Network> {
        let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .context("bind party endpoint")?;
        // The ticket carries relay and direct addresses; do not wait forever
        // for the relay when LAN-only connectivity is sufficient.
        let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.online()).await;
        let router = Router::builder(endpoint.clone())
            .accept(
                ALPN,
                Handler {
                    shared: self.shared.clone(),
                    connections: Arc::new(Semaphore::new(16)),
                },
            )
            .spawn();
        Ok(Network { endpoint, router })
    }

    pub fn create(
        &self,
        media: PartyMedia,
        mut report: LocalReport,
        name: String,
    ) -> Result<String> {
        engine::validate_media(&media)?;
        validate_name(&name)?;
        ensure!(
            report.loaded && report.seekable && !report.ended,
            "load a seekable stream before hosting"
        );
        ensure!(
            engine::duration_matches(media.duration, report.duration),
            "host media duration differs"
        );
        report.generation = 1;
        if report.media.is_none() {
            report.media = Some(media.identity.clone());
        }
        ensure!(
            engine::media_matches(&media.identity, report.media.as_ref()),
            "host media differs"
        );
        engine::validate_report(&report)?;
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        ensure!(session.network.is_none(), "already in a party");
        let runtime = self.runtime()?;
        let network = runtime.block_on(self.bind())?;
        let ticket = Ticket {
            host: network.endpoint.addr(),
            room_id: ticket::random_hex(),
            capability: iroh::SecretKey::generate().to_bytes(),
            expires_unix: ticket::unix_now() + engine::INVITE_LIFETIME_MS / 1_000,
        };
        let invite = match ticket.encode() {
            Ok(invite) => invite,
            Err(error) => {
                runtime.block_on(network.router.shutdown())?;
                return Err(error);
            }
        };
        let room = HostRoom::new(
            ticket.room_id,
            network.endpoint.id().to_string(),
            ticket.capability,
            media,
            report,
            name,
            local_now_ms(),
        );
        self.shared.publish(room.snapshot.clone());
        *self.shared.host.lock().unwrap_or_else(|e| e.into_inner()) = Some(room);
        session.network = Some(network);
        Ok(invite)
    }

    pub fn join(&self, invite: &str, name: String) -> Result<()> {
        validate_name(&name)?;
        let ticket = Ticket::decode(invite)?;
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        ensure!(session.network.is_none(), "already in a party");
        let runtime = self.runtime()?;
        let network = runtime.block_on(self.bind())?;
        ensure!(
            network.endpoint.id() != ticket.host.id,
            "host cannot join itself"
        );
        let mut guest = Guest {
            ticket,
            connection: None,
            next_sequence: 1,
            last_success: local_now_ms(),
            best_rtt: f64::INFINITY,
            best_sample_at: 0,
            offset: 0.0,
        };
        let result = runtime.block_on(exchange(
            &network.endpoint,
            &mut guest,
            Operation::Join { name },
        ));
        match result {
            Ok(Some(snapshot)) => {
                self.shared.publish(snapshot);
                session.network = Some(network);
                session.guest = Some(guest);
                Ok(())
            }
            result => {
                let _ = runtime.block_on(network.router.shutdown());
                result?.context("missing party snapshot")?;
                anyhow::bail!("missing party snapshot")
            }
        }
    }

    /// A try-read keeps the UI path independent of transport and host work.
    pub fn snapshot(&self) -> Option<RoomSnapshot> {
        self.shared.snapshot.try_read().ok().and_then(|s| s.clone())
    }

    pub fn report(&self, report: LocalReport) -> Result<()> {
        engine::validate_report(&report)?;
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let Session { network, guest } = &mut *session;
        let network = network.as_ref().context("not in a party")?;
        if let Some(guest) = guest {
            ensure!(
                !self.snapshot().is_some_and(|snapshot| snapshot.ended),
                "party has ended"
            );
            if local_now_ms().saturating_sub(guest.last_success) >= HOST_TIMEOUT_MS {
                if let Some(mut snapshot) = self.snapshot() {
                    pause_disconnected(&mut snapshot, local_now_ms());
                    snapshot.ended = true;
                    snapshot.error = Some("host disconnected; party ended".into());
                    self.shared.publish(snapshot);
                }
                anyhow::bail!("host disconnected; party ended");
            }
            let result = self.runtime()?.block_on(exchange(
                &network.endpoint,
                guest,
                Operation::Report(report),
            ));
            match result {
                Ok(Some(snapshot)) => {
                    self.shared.publish(snapshot);
                    Ok(())
                }
                result => {
                    if let Some(mut snapshot) = self.snapshot() {
                        pause_disconnected(&mut snapshot, local_now_ms());
                        snapshot.ended |=
                            local_now_ms().saturating_sub(guest.last_success) >= HOST_TIMEOUT_MS;
                        let terminal = result
                            .as_ref()
                            .err()
                            .and_then(|e| e.downcast_ref::<TerminalError>());
                        snapshot.ended |= terminal.is_some();
                        snapshot.error = Some(terminal.map_or_else(
                            || {
                                if snapshot.ended {
                                    "host disconnected; party ended".into()
                                } else {
                                    "host connection lost; paused while reconnecting".into()
                                }
                            },
                            |error| error.0.clone(),
                        ));
                        self.shared.publish(snapshot);
                    }
                    result?.context("missing party snapshot")?;
                    anyhow::bail!("missing party snapshot")
                }
            }
        } else {
            self.with_host(|room| room.local_report(report, local_now_ms()))
        }
    }

    fn with_host(&self, operation: impl FnOnce(&mut HostRoom) -> Result<()>) -> Result<()> {
        let mut host = self.shared.host.lock().unwrap_or_else(|e| e.into_inner());
        let room = host
            .as_mut()
            .context("only the host can change the party timeline")?;
        operation(room)?;
        self.shared.publish(room.snapshot.clone());
        Ok(())
    }

    pub fn command(&self, command: HostCommand) -> Result<()> {
        self.with_host(|room| room.command(command, local_now_ms()))
    }

    pub fn kick(&self, member_id: &str) -> Result<()> {
        self.with_host(|room| room.remove(member_id, local_now_ms()))
    }

    pub fn continue_without_waiting(&self) -> Result<()> {
        self.with_host(|room| {
            room.continue_without_waiting(local_now_ms());
            Ok(())
        })
    }

    pub fn set_wait_for_guests(&self, enabled: bool) -> Result<()> {
        self.with_host(|room| {
            room.snapshot.wait_for_guests = enabled;
            room.tick(local_now_ms());
            Ok(())
        })
    }

    pub fn leave(&self) -> Result<()> {
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        // Revoke before awaiting shutdown; already accepted frames must fail.
        *self.shared.host.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Some(network) = session.network.take() {
            let runtime = self.runtime()?;
            if let Some(mut guest) = session.guest.take() {
                let _ = runtime.block_on(exchange(&network.endpoint, &mut guest, Operation::Leave));
            }
            let _ = runtime.block_on(network.router.shutdown());
        }
        *self
            .shared
            .snapshot
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty() && name.len() <= 64 && !name.chars().any(char::is_control),
        "party name must contain 1–64 bytes of text"
    );
    Ok(())
}

async fn exchange(
    endpoint: &Endpoint,
    guest: &mut Guest,
    mut operation: Operation,
) -> Result<Option<RoomSnapshot>> {
    if let Operation::Report(report) = &mut operation {
        // Local errors can embed authenticated URLs; peers only need the fact
        // that this participant cannot currently play its chosen source.
        if report.error.is_some() {
            report.error = Some("local source unavailable".into());
        }
    }
    let sequence = guest.next_sequence;
    guest.next_sequence = sequence
        .checked_add(1)
        .context("party sequence exhausted")?;
    let request = Request {
        room_id: guest.ticket.room_id.clone(),
        capability: guest.ticket.capability,
        sequence,
        operation,
    };
    let started = local_now_ms();
    let result = tokio::time::timeout(EXCHANGE_TIMEOUT, async {
        if guest.connection.is_none() {
            guest.connection = Some(endpoint.connect(guest.ticket.host.clone(), ALPN).await?);
        }
        let connection = guest.connection.as_ref().expect("connected");
        ensure!(
            connection.remote_id() == guest.ticket.host.id,
            "wrong party host"
        );
        let (mut send, mut recv) = connection.open_bi().await?;
        wire::write(&mut send, &request).await?;
        let response: Response = wire::read(&mut recv).await?;
        ensure!(response.sequence == sequence, "mismatched party response");
        if let Some(error) = response.error {
            if response.terminal {
                return Err(TerminalError(error).into());
            }
            anyhow::bail!("{error}");
        }
        let finished = local_now_ms();
        let rtt = finished.saturating_sub(started) as f64;
        // Use the least queued sample; RTT midpoint bounds offset uncertainty
        // without trusting wall clocks or assuming symmetric one-way latency.
        if rtt <= guest.best_rtt || finished.saturating_sub(guest.best_sample_at) >= 10_000 {
            guest.best_rtt = rtt;
            guest.best_sample_at = finished;
            guest.offset = midpoint_offset(response.host_now_ms, started, finished);
        }
        let snapshot = response.snapshot.map(|mut snapshot| {
            snapshot.role = PartyRole::Guest;
            snapshot.member_id = endpoint.id().to_string();
            snapshot.clock_offset_ms = guest.offset;
            snapshot.connected = true;
            snapshot
        });
        if let Some(snapshot) = &snapshot {
            ensure!(
                snapshot.room_id == guest.ticket.room_id
                    && snapshot.host_id == guest.ticket.host.id.to_string()
                    && snapshot.participants.len() <= MAX_MEMBERS
                    && snapshot.generation == 1,
                "invalid party snapshot"
            );
            engine::validate_media(&snapshot.media)?;
            ensure!(
                snapshot.timeline.position.is_finite() && snapshot.timeline.position >= 0.0,
                "invalid party timeline"
            );
        }
        guest.last_success = finished;
        Ok(snapshot)
    })
    .await
    .context("party request timed out")
    .and_then(|result| result);
    if result.is_err()
        && let Some(connection) = guest.connection.take()
    {
        connection.close(0u32.into(), b"party request failed");
    }
    result
}

fn midpoint_offset(host_ms: u64, started: u64, finished: u64) -> f64 {
    host_ms as f64 - (started as f64 + finished.saturating_sub(started) as f64 / 2.0)
}

#[derive(Clone)]
struct Handler {
    shared: Arc<Shared>,
    connections: Arc<Semaphore>,
}

impl fmt::Debug for Handler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PartyHandler")
    }
}

impl Handler {
    fn dispatch(&self, peer: &str, request: Request) -> Response {
        let mut host = self.shared.host.lock().unwrap_or_else(|e| e.into_inner());
        let mut terminal = false;
        let result = (|| -> Result<Option<RoomSnapshot>> {
            let room = host.as_mut().ok_or_else(|| {
                terminal = true;
                anyhow::anyhow!("party has ended")
            })?;
            let now = local_now_ms();
            if let Err(error) = room.authorize(&request.room_id, &request.capability, peer, now) {
                terminal = true;
                return Err(error);
            }
            if !matches!(&request.operation, Operation::Join { .. })
                && !room.members.contains_key(peer)
            {
                terminal = true;
                anyhow::bail!("party membership revoked");
            }
            ensure!(request.sequence != 0, "invalid party sequence");
            match request.operation {
                Operation::Join { name } => {
                    validate_name(&name)?;
                    room.join(peer, name, request.sequence, now)?;
                }
                Operation::Report(report) => room.report(peer, report, request.sequence, now)?,
                Operation::Leave => {
                    ensure!(
                        room.members
                            .get(peer)
                            .is_some_and(|m| request.sequence > m.last_sequence),
                        "stale leave request"
                    );
                    room.remove(peer, now)?;
                    self.shared.publish(room.snapshot.clone());
                    return Ok(None);
                }
            }
            room.tick(now);
            self.shared.publish(room.snapshot.clone());
            Ok(Some(room.snapshot.clone()))
        })();
        match result {
            Ok(snapshot) => Response {
                sequence: request.sequence,
                host_now_ms: local_now_ms(),
                snapshot,
                error: None,
                terminal: false,
            },
            Err(error) => Response {
                sequence: request.sequence,
                host_now_ms: local_now_ms(),
                snapshot: None,
                error: Some(error.to_string().chars().take(256).collect()),
                terminal,
            },
        }
    }
}

impl ProtocolHandler for Handler {
    async fn accept(&self, connection: Connection) -> Result<(), iroh::protocol::AcceptError> {
        let Ok(_permit) = self.connections.clone().try_acquire_owned() else {
            connection.close(0u32.into(), b"party busy");
            return Ok(());
        };
        let peer = connection.remote_id().to_string();
        loop {
            let streams =
                tokio::time::timeout(Duration::from_secs(5), connection.accept_bi()).await;
            let Ok(Ok((mut send, mut recv))) = streams else {
                break;
            };
            let result = tokio::time::timeout(EXCHANGE_TIMEOUT, async {
                let request: Request = wire::read(&mut recv).await?;
                let response = self.dispatch(&peer, request);
                wire::write(&mut send, &response).await
            })
            .await;
            if !matches!(result, Ok(Ok(()))) {
                break;
            }
        }
        connection.close(0u32.into(), b"party connection ended");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn endpoint() -> Endpoint {
        Endpoint::builder(iroh::endpoint::presets::Minimal)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .unwrap()
    }

    async fn host() -> (Endpoint, Router, Handler, Ticket) {
        let endpoint = endpoint().await;
        let shared = Arc::new(Shared::default());
        let room_id = ticket::random_hex();
        let ticket = Ticket {
            host: endpoint.addr(),
            room_id: room_id.clone(),
            capability: [42; 32],
            expires_unix: ticket::unix_now() + 60,
        };
        *shared.host.lock().unwrap() = Some(HostRoom::new(
            room_id,
            endpoint.id().to_string(),
            ticket.capability,
            engine::tests::media(),
            engine::tests::report(),
            "Host".into(),
            local_now_ms(),
        ));
        let handler = Handler {
            shared,
            connections: Arc::new(Semaphore::new(16)),
        };
        let router = Router::builder(endpoint.clone())
            .accept(ALPN, handler.clone())
            .spawn();
        (endpoint, router, handler, ticket)
    }

    fn guest(ticket: Ticket) -> Guest {
        Guest {
            ticket,
            connection: None,
            next_sequence: 1,
            last_success: local_now_ms(),
            best_rtt: f64::INFINITY,
            best_sample_at: 0,
            offset: 0.0,
        }
    }

    #[test]
    fn clock_uses_rtt_midpoint_without_wall_time() {
        assert_eq!(midpoint_offset(2_250, 1_000, 1_500), 1_000.0);
        assert_eq!(midpoint_offset(250, 1_000, 1_500), -1_000.0);
        assert_eq!(midpoint_offset(100, 100, 100), 0.0);
    }

    #[test]
    fn idle_service_is_lazy_and_disconnect_freezes_position() {
        let service = PartyService::new().unwrap();
        assert!(service.runtime.get().is_none());
        assert!(service.snapshot().is_none());
        assert!(service.command(HostCommand::SetPaused(false)).is_err());
        service.leave().unwrap();
        assert!(service.runtime.get().is_none());
        let mut snapshot = engine::tests::room().snapshot;
        pause_disconnected(&mut snapshot, 2_000);
        assert!(!snapshot.connected);
        assert!(snapshot.timeline.paused);
        assert_eq!(snapshot.timeline.position_at(9_000.0), 11.0);
    }

    #[tokio::test]
    async fn two_unpaired_endpoints_share_only_timeline_and_revocation_survives_reconnect() {
        let (host, router, handler, ticket) = host().await;
        let endpoint = endpoint().await;
        let mut guest = guest(ticket);
        let joined = exchange(
            &endpoint,
            &mut guest,
            Operation::Join {
                name: "Guest".into(),
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(joined.role, PartyRole::Guest);
        assert_eq!(joined.host_id, host.id().to_string());
        assert_eq!(joined.member_id, endpoint.id().to_string());
        assert!(joined.waiting.is_empty());
        assert!(
            !joined
                .participants
                .iter()
                .find(|p| !p.is_host)
                .unwrap()
                .ready
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
        let ready = exchange(
            &endpoint,
            &mut guest,
            Operation::Report(engine::tests::report()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            ready
                .participants
                .iter()
                .find(|p| !p.is_host)
                .unwrap()
                .ready
        );
        // Losing the transport does not lose membership or create a sync peer.
        guest
            .connection
            .take()
            .unwrap()
            .close(0u32.into(), b"test reconnect");
        tokio::time::sleep(Duration::from_millis(60)).await;
        handler
            .shared
            .host
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .command(HostCommand::SeekAbsolute(80.0), local_now_ms())
            .unwrap();
        let fresh = exchange(
            &endpoint,
            &mut guest,
            Operation::Report(engine::tests::report()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(fresh.timeline.position, 80.0);
        assert!(fresh.revision > ready.revision);
        assert!(fresh.clock_offset_ms.abs() < 1_000.0);
        let queued = Request {
            room_id: guest.ticket.room_id.clone(),
            capability: guest.ticket.capability,
            sequence: 999,
            operation: Operation::Report(engine::tests::report()),
        };
        handler
            .shared
            .host
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .remove(&endpoint.id().to_string(), local_now_ms())
            .unwrap();
        let denied = handler.dispatch(&endpoint.id().to_string(), queued);
        assert!(denied.error.is_some() && denied.terminal);
        let denied = exchange(
            &endpoint,
            &mut guest,
            Operation::Report(engine::tests::report()),
        )
        .await
        .unwrap_err();
        assert!(denied.downcast_ref::<TerminalError>().is_some());
        assert!(
            exchange(
                &endpoint,
                &mut guest,
                Operation::Join {
                    name: "Again".into()
                }
            )
            .await
            .is_err()
        );
        assert!(
            tokio::time::timeout(
                EXCHANGE_TIMEOUT,
                endpoint.connect(host.addr(), b"nova/sync/3")
            )
            .await
            .unwrap()
            .is_err()
        );
        endpoint.close().await;
        router.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn wrong_capability_and_oversized_streams_never_admit_members() {
        let (host, router, handler, mut ticket) = host().await;
        let endpoint = endpoint().await;
        ticket.capability[0] ^= 1;
        let mut guest = guest(ticket);
        assert!(
            exchange(
                &endpoint,
                &mut guest,
                Operation::Join {
                    name: "Intruder".into()
                }
            )
            .await
            .is_err()
        );
        let connection = endpoint.connect(host.addr(), ALPN).await.unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        send.write_all(&vec![0; wire::MAX_FRAME + 1]).await.unwrap();
        send.finish().unwrap();
        assert!(
            tokio::time::timeout(EXCHANGE_TIMEOUT, wire::read::<Response>(&mut recv))
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(
            handler
                .shared
                .host
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .members
                .len(),
            1
        );
        endpoint.close().await;
        router.shutdown().await.unwrap();
    }
}

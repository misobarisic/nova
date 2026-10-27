//! Blocking foreground-worker API around an independent, lazily bound endpoint.
use crate::engine::{self, HostRoom};
use crate::ticket::{self, Ticket};
use crate::wire::{self, ControlIntent, Operation, Request, Response};
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

#[derive(Debug)]
struct NonterminalError {
    message: String,
    snapshot: Option<Box<RoomSnapshot>>,
}

impl fmt::Display for NonterminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for NonterminalError {}

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
        .clamp(
            0.0,
            snapshot.media.as_ref().map_or(0.0, |media| media.duration),
        );
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
    next_control_sequence: u64,
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
        self.create_room(Some(media), report, name)
    }

    /// Invite people before choosing a show or a local playback source.
    pub fn create_lobby(&self, name: String) -> Result<String> {
        validate_name(&name)?;
        self.create_room(None, LocalReport::default(), name)
    }

    fn create_room(
        &self,
        media: Option<PartyMedia>,
        report: LocalReport,
        name: String,
    ) -> Result<String> {
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
        let host_id = network.endpoint.id().to_string();
        let now = local_now_ms();
        let room = if let Some(media) = media {
            HostRoom::new(
                ticket.room_id,
                host_id,
                ticket.capability,
                media,
                report,
                name,
                now,
            )
        } else {
            HostRoom::lobby(ticket.room_id, host_id, ticket.capability, name, now)
        };
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
            next_control_sequence: 1,
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
                    if let Err(error) = &result
                        && self.publish_rejection(error)
                    {
                        return result.map(|_| ());
                    }
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
        let result = operation(room);
        // Expiry checks may end the room even when the requested operation is
        // rejected; publish that terminal state as well.
        self.shared.publish(room.snapshot.clone());
        result
    }

    pub fn command(&self, command: HostCommand) -> Result<()> {
        self.with_host(|room| room.command(command, local_now_ms()))
    }

    /// Apply a queued intent only to the media it was captured for.
    pub fn command_for_generation(&self, generation: u64, command: HostCommand) -> Result<()> {
        let revision = self.snapshot().context("not in a party")?.control_revision;
        self.command_for_state(generation, revision, command)
    }

    /// Playback and media selection use the same live room policy and guards.
    pub fn command_for_state(
        &self,
        generation: u64,
        control_revision: u64,
        command: HostCommand,
    ) -> Result<()> {
        self.control(
            generation,
            control_revision,
            ControlIntent::Playback(command),
        )
    }

    pub fn propose_media(
        &self,
        generation: u64,
        control_revision: u64,
        mut media: PartyMedia,
    ) -> Result<()> {
        media.duration = 0.0;
        self.control(
            generation,
            control_revision,
            ControlIntent::ProposeMedia(media),
        )
    }

    fn publish_rejection(&self, error: &anyhow::Error) -> bool {
        if let Some(rejection) = error.downcast_ref::<ControlRejection>() {
            self.shared.publish(*rejection.snapshot.clone());
            true
        } else if let Some(rejection) = error.downcast_ref::<NonterminalError>() {
            if let Some(snapshot) = &rejection.snapshot {
                self.shared.publish(*snapshot.clone());
            }
            true
        } else {
            false
        }
    }

    fn control(&self, generation: u64, control_revision: u64, intent: ControlIntent) -> Result<()> {
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let Session { network, guest } = &mut *session;
        if let Some(guest) = guest {
            let network = network.as_ref().context("not in a party")?;
            let result = self.runtime()?.block_on(exchange(
                &network.endpoint,
                guest,
                Operation::Control {
                    generation,
                    control_revision,
                    intent,
                },
            ));
            match result {
                Ok(Some(snapshot)) => {
                    self.shared.publish(snapshot);
                    Ok(())
                }
                Ok(None) => anyhow::bail!("missing party snapshot"),
                Err(error) => {
                    self.publish_rejection(&error);
                    Err(error)
                }
            }
        } else {
            self.with_host(|room| {
                let id = room.snapshot.host_id.clone();
                room.control(
                    &id,
                    generation,
                    control_revision,
                    None,
                    intent,
                    local_now_ms(),
                )
            })
        }
    }

    pub fn set_control_policy(&self, policy: ControlPolicy) -> Result<()> {
        self.with_host(|room| room.set_control_policy(policy, local_now_ms()))
    }

    /// Replace the shared selection without changing invites or membership.
    pub fn prepare_media(&self, media: PartyMedia) -> Result<()> {
        self.with_host(|room| room.prepare_media(media, local_now_ms()))
    }

    /// Validate the host source and begin the selected episode's readiness barrier.
    pub fn select_media(&self, media: PartyMedia, report: LocalReport) -> Result<()> {
        self.with_host(|room| room.select_media(media, report, local_now_ms()))
    }

    /// Bind validation to the accepted selection, including repeated A–B–A identities.
    pub fn select_media_for_generation(
        &self,
        generation: u64,
        media: PartyMedia,
        report: LocalReport,
    ) -> Result<()> {
        self.with_host(|room| {
            room.select_media_for_generation(generation, media, report, local_now_ms())
        })
    }

    /// Keep the room open while the host browses for its next selection.
    pub fn clear_media(&self) -> Result<()> {
        self.with_host(|room| room.clear_media(local_now_ms()))
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
    let counter = if matches!(operation, Operation::Control { .. }) {
        &mut guest.next_control_sequence
    } else {
        &mut guest.next_sequence
    };
    let sequence = *counter;
    *counter = sequence
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
        if response.terminal {
            return Err(
                TerminalError(response.error.unwrap_or_else(|| "party has ended".into())).into(),
            );
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
                    && snapshot.generation >= 1,
                "invalid party snapshot"
            );
            if let Some(media) = &snapshot.media {
                engine::validate_media(media)?;
            } else {
                ensure!(
                    snapshot.timeline.paused
                        && snapshot.timeline.position == 0.0
                        && snapshot.waiting.is_empty(),
                    "invalid party lobby"
                );
            }
            if let Some(pending) = &snapshot.pending_media {
                ensure!(
                    engine::valid_identity(&pending.identity)
                        && pending.duration == 0.0
                        && snapshot.media.is_none(),
                    "invalid pending party media"
                );
            }
            ensure!(
                snapshot.timeline.position.is_finite()
                    && snapshot.timeline.position >= 0.0
                    && engine::valid_target_rate(snapshot.timeline.target_rate),
                "invalid party timeline"
            );
        }
        guest.last_success = finished;
        if let Some(reason) = response.rejection {
            return Err(ControlRejection {
                reason,
                snapshot: Box::new(snapshot.context("missing rejected party snapshot")?),
            }
            .into());
        }
        if let Some(message) = response.error {
            return Err(NonterminalError {
                message,
                snapshot: snapshot.map(Box::new),
            }
            .into());
        }
        Ok(snapshot)
    })
    .await
    .context("party request timed out")
    .and_then(|result| result);
    if result.as_ref().err().is_some_and(|error| {
        error.downcast_ref::<ControlRejection>().is_none()
            && error.downcast_ref::<NonterminalError>().is_none()
    }) && let Some(connection) = guest.connection.take()
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
                Operation::Control {
                    generation,
                    control_revision,
                    intent,
                } => {
                    room.control(
                        peer,
                        generation,
                        control_revision,
                        Some(request.sequence),
                        intent,
                        now,
                    )?;
                }
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
                rejection: None,
            },
            Err(error) => {
                let snapshot = if terminal {
                    None
                } else {
                    host.as_ref().map(|room| room.snapshot.clone())
                };
                if let Some(snapshot) = &snapshot {
                    self.shared.publish(snapshot.clone());
                }
                Response {
                    sequence: request.sequence,
                    host_now_ms: local_now_ms(),
                    snapshot,
                    error: Some(error.to_string().chars().take(256).collect()),
                    terminal,
                    rejection: error
                        .downcast_ref::<ControlRejection>()
                        .map(|rejection| rejection.reason),
                }
            }
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
            next_control_sequence: 1,
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

    #[test]
    fn only_host_selects_media_and_queued_commands_cannot_cross_generations() {
        let guest = PartyService::new().unwrap();
        let mut snapshot = engine::tests::room().snapshot;
        snapshot.role = PartyRole::Guest;
        guest.shared.publish(snapshot);
        assert!(
            guest
                .select_media(engine::tests::media(), engine::tests::report())
                .is_err()
        );
        assert!(guest.clear_media().is_err());
        assert!(guest.runtime.get().is_none());

        let host = PartyService::new().unwrap();
        *host.shared.host.lock().unwrap() = Some(HostRoom::lobby(
            "a".repeat(32),
            "host".into(),
            [7; 32],
            "Host".into(),
            local_now_ms(),
        ));
        host.select_media(engine::tests::media(), engine::tests::report())
            .unwrap();
        let generation = host.snapshot().unwrap().generation;
        host.clear_media().unwrap();
        host.select_media(engine::tests::media(), engine::tests::report())
            .unwrap();
        assert!(
            host.command_for_generation(generation, HostCommand::SeekAbsolute(90.0))
                .is_err()
        );
        assert_eq!(host.snapshot().unwrap().timeline.position, 10.0);
        let current = host.snapshot().unwrap().generation;
        host.command_for_generation(current, HostCommand::SeekAbsolute(20.0))
            .unwrap();
        assert_eq!(host.snapshot().unwrap().timeline.position, 20.0);
    }

    #[tokio::test]
    async fn guest_controls_share_policy_and_business_rejections_keep_the_connection() {
        let (_host, router, handler, ticket) = host().await;
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
        let control = |snapshot: &RoomSnapshot, intent| Operation::Control {
            generation: snapshot.generation,
            control_revision: snapshot.control_revision,
            intent,
        };
        let denied = exchange(
            &endpoint,
            &mut guest,
            control(
                &joined,
                ControlIntent::Playback(HostCommand::SeekAbsolute(40.0)),
            ),
        )
        .await
        .unwrap_err();
        let denied = denied.downcast_ref::<ControlRejection>().unwrap();
        assert_eq!(denied.reason, ControlRejectionReason::PermissionDenied);
        assert_eq!(denied.snapshot.role, PartyRole::Guest);
        assert!(denied.snapshot.connected && !denied.snapshot.ended);
        assert!(guest.connection.is_some());
        assert_eq!(denied.snapshot.participants.len(), 2);
        handler
            .shared
            .host
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .set_control_policy(ControlPolicy::Everyone, local_now_ms())
            .unwrap();
        let fresh = exchange(
            &endpoint,
            &mut guest,
            Operation::Report(engine::tests::report()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(fresh.control_policy, ControlPolicy::Everyone);
        tokio::time::sleep(Duration::from_millis(60)).await;
        let moved = exchange(
            &endpoint,
            &mut guest,
            control(
                &fresh,
                ControlIntent::Playback(HostCommand::SeekAbsolute(40.0)),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(moved.timeline.position, 40.0);
        // A duplicate request can arrive over another stream, including after
        // a response was lost. A relative command must never apply twice.
        let replay = Request {
            room_id: guest.ticket.room_id.clone(),
            capability: guest.ticket.capability,
            sequence: guest.next_control_sequence - 1,
            operation: control(
                &fresh,
                ControlIntent::Playback(HostCommand::SeekRelative(10.0)),
            ),
        };
        let (mut send, mut recv) = guest.connection.as_ref().unwrap().open_bi().await.unwrap();
        wire::write(&mut send, &replay).await.unwrap();
        let replayed: Response = wire::read(&mut recv).await.unwrap();
        assert_eq!(replayed.rejection, Some(ControlRejectionReason::Duplicate));
        assert!(!replayed.terminal);
        assert_eq!(replayed.snapshot.unwrap().timeline.position, 40.0);
        handler
            .shared
            .host
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .set_control_policy(ControlPolicy::HostOnly, local_now_ms())
            .unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        let stale = exchange(
            &endpoint,
            &mut guest,
            control(
                &fresh,
                ControlIntent::Playback(HostCommand::SeekRelative(10.0)),
            ),
        )
        .await
        .unwrap_err();
        let stale = stale.downcast_ref::<ControlRejection>().unwrap();
        assert_eq!(stale.reason, ControlRejectionReason::StaleControlRevision);
        assert_eq!(stale.snapshot.control_policy, ControlPolicy::HostOnly);
        assert!(guest.connection.is_some());
        handler
            .shared
            .host
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .set_control_policy(ControlPolicy::Everyone, local_now_ms())
            .unwrap();
        let current = exchange(
            &endpoint,
            &mut guest,
            Operation::Report(engine::tests::report()),
        )
        .await
        .unwrap()
        .unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        let pending = exchange(
            &endpoint,
            &mut guest,
            control(
                &current,
                ControlIntent::ProposeMedia(PartyMedia {
                    duration: 0.0,
                    ..engine::tests::media()
                }),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(pending.generation, current.generation + 1);
        assert!(pending.media.is_none() && pending.pending_media.is_some());
        assert_eq!(pending.pending_media.as_ref().unwrap().duration, 0.0);
        // A proposal publishes identity only; the host must independently load
        // and validate a local source before any shared playback can start.
        let ready = LocalReport {
            generation: pending.generation,
            ..engine::tests::report()
        };
        tokio::time::sleep(Duration::from_millis(60)).await;
        exchange(&endpoint, &mut guest, Operation::Report(ready.clone()))
            .await
            .unwrap();
        handler
            .shared
            .host
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .select_media_for_generation(
                pending.generation,
                engine::tests::media(),
                ready.clone(),
                local_now_ms(),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        let active = exchange(&endpoint, &mut guest, Operation::Report(ready))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(active.generation, pending.generation);
        assert!(
            active
                .participants
                .iter()
                .find(|p| p.id == endpoint.id().to_string())
                .unwrap()
                .ready
        );
        handler
            .shared
            .host
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .remove(&endpoint.id().to_string(), local_now_ms())
            .unwrap();
        let revoked = exchange(
            &endpoint,
            &mut guest,
            control(
                &active,
                ControlIntent::Playback(HostCommand::SetPaused(true)),
            ),
        )
        .await
        .unwrap_err();
        assert!(revoked.downcast_ref::<TerminalError>().is_some());
        assert!(guest.connection.is_none());
        endpoint.close().await;
        router.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn guests_share_room_speed_and_step_replays_do_not_double_apply() {
        let (_host, router, handler, ticket) = host().await;
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
        handler
            .shared
            .host
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .set_control_policy(ControlPolicy::Everyone, local_now_ms())
            .unwrap();
        let mut current = exchange(
            &endpoint,
            &mut guest,
            Operation::Report(engine::tests::report()),
        )
        .await
        .unwrap()
        .unwrap();
        let operation = |snapshot: &RoomSnapshot, command| Operation::Control {
            generation: snapshot.generation,
            control_revision: snapshot.control_revision,
            intent: ControlIntent::Playback(command),
        };
        current = exchange(
            &endpoint,
            &mut guest,
            operation(&current, HostCommand::SetRate(1.5)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(current.timeline.target_rate, 1.5);
        tokio::time::sleep(Duration::from_millis(60)).await;
        let before_step = current.clone();
        current = exchange(
            &endpoint,
            &mut guest,
            operation(&current, HostCommand::StepRate(1)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(current.timeline.target_rate, 1.55);
        assert_eq!(current.control_revision, before_step.control_revision);
        let replay = Request {
            room_id: guest.ticket.room_id.clone(),
            capability: guest.ticket.capability,
            sequence: guest.next_control_sequence - 1,
            operation: operation(&before_step, HostCommand::StepRate(1)),
        };
        let (mut send, mut recv) = guest.connection.as_ref().unwrap().open_bi().await.unwrap();
        wire::write(&mut send, &replay).await.unwrap();
        let response: Response = wire::read(&mut recv).await.unwrap();
        assert_eq!(response.rejection, Some(ControlRejectionReason::Duplicate));
        assert_eq!(response.snapshot.unwrap().timeline.target_rate, 1.55);
        assert!(guest.connection.is_some());
        assert_eq!(joined.timeline.target_rate, 1.0);
        handler
            .shared
            .host
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .clear_media(local_now_ms())
            .unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        current = exchange(
            &endpoint,
            &mut guest,
            Operation::Report(LocalReport::default()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(current.media.is_none());
        assert_eq!(current.timeline.target_rate, 1.55);
        current = exchange(
            &endpoint,
            &mut guest,
            operation(&current, HostCommand::ResetRate),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(current.timeline.target_rate, 1.0);
        assert!(current.media.is_none() && current.timeline.paused);
        endpoint.close().await;
        router.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn guest_joins_lobby_and_receives_replacements_through_the_same_invite() {
        let (host, router, handler, ticket) = host().await;
        handler
            .shared
            .host
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .clear_media(local_now_ms())
            .unwrap();
        let endpoint = endpoint().await;
        let mut guest = guest(ticket);
        let lobby = exchange(
            &endpoint,
            &mut guest,
            Operation::Join {
                name: "Guest".into(),
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert!(lobby.media.is_none() && lobby.timeline.paused && lobby.waiting.is_empty());
        assert!(!lobby.ended);
        handler
            .shared
            .host
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .select_media(
                engine::tests::media(),
                engine::tests::report(),
                local_now_ms(),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        let selected = exchange(
            &endpoint,
            &mut guest,
            Operation::Report(LocalReport::default()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(selected.media, Some(engine::tests::media()));
        assert!(selected.generation > lobby.generation);
        let mut next = engine::tests::media();
        next.identity.episode_id = "next".into();
        let next_report = LocalReport {
            media: Some(next.identity.clone()),
            position: 0.0,
            ..engine::tests::report()
        };
        handler
            .shared
            .host
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .select_media(next.clone(), next_report, local_now_ms())
            .unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        let stale = LocalReport {
            generation: selected.generation,
            ended: true,
            ..engine::tests::report()
        };
        let replaced = exchange(&endpoint, &mut guest, Operation::Report(stale))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replaced.media, Some(next));
        assert_eq!(replaced.room_id, lobby.room_id);
        assert_eq!(replaced.host_id, host.id().to_string());
        assert_eq!(replaced.member_id, lobby.member_id);
        assert_eq!(replaced.participants.len(), 2);
        assert!(!replaced.ended);
        assert_eq!(replaced.waiting, ["Guest"]);
        assert!(replaced.timeline.paused);
        assert_eq!(replaced.timeline.position, 0.0);
        assert!(
            !replaced
                .participants
                .iter()
                .find(|p| !p.is_host)
                .unwrap()
                .ready
        );
        endpoint.close().await;
        router.shutdown().await.unwrap();
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

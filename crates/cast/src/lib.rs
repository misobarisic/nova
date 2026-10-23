//! Bounded LAN receiver discovery and ownership-checked control.
//! Receiver credentials and token-bearing media URLs deliberately do not implement Debug.
mod discovery;
mod dlna;
mod google;

pub use discovery::{discover, discover_manual};
use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    GoogleCast,
    Dlna,
}
#[derive(Clone)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub address: SocketAddr,
    pub receiver_ip: IpAddr,
    pub video: bool,
    pub model: Option<String>,
    pub(crate) endpoint: Option<dlna::Description>,
}
impl fmt::Debug for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Device")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("protocol", &self.protocol)
            .field("address", &self.address)
            .field("video", &self.video)
            .finish()
    }
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReceiverCapabilities {
    pub seek: bool,
    pub pause: bool,
    pub volume: bool,
    pub mute: bool,
    pub text_tracks: bool,
    /// Empty means unknown. These are receiver advertisements, not a decoder guarantee.
    pub mime_types: Vec<String>,
    pub codecs: Vec<String>,
}
#[derive(Clone)]
pub struct TextTrack {
    pub id: i64,
    pub url: String,
    pub name: String,
    pub language: String,
    pub mime_type: String,
}
#[derive(Clone)]
pub struct MediaLoad {
    pub url: String,
    pub mime_type: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub duration: Option<f64>,
    pub start_position: f64,
    pub paused: bool,
    pub text_tracks: Vec<TextTrack>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReceiverCommand {
    SetPaused(bool),
    Seek(f64),
    SetVolume(f32),
    SetMuted(bool),
    SetTextTrack(Option<i64>),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Idle,
    Playing,
    Paused,
    Buffering,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    Finished,
    Cancelled,
    Interrupted,
    Error,
    Takeover,
}
#[derive(Clone)]
pub struct ReceiverSnapshot {
    pub app_session: Option<String>,
    pub media_session: Option<String>,
    pub content_id: Option<String>,
    pub state: PlaybackState,
    pub position: f64,
    pub duration: Option<f64>,
    pub volume: Option<f32>,
    pub muted: Option<bool>,
    pub capabilities: ReceiverCapabilities,
    pub observed_at: Instant,
    pub end_reason: Option<EndReason>,
    pub owned: bool,
}
impl Default for ReceiverSnapshot {
    fn default() -> Self {
        Self {
            app_session: None,
            media_session: None,
            content_id: None,
            state: PlaybackState::Unknown,
            position: 0.,
            duration: None,
            volume: None,
            muted: None,
            capabilities: ReceiverCapabilities::default(),
            observed_at: Instant::now(),
            end_reason: None,
            owned: false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastErrorKind {
    Cancelled,
    Timeout,
    Network,
    Protocol,
    Unsupported,
    Takeover,
    ReplacementRequired,
    InvalidInput,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CastError {
    pub kind: CastErrorKind,
}
impl CastError {
    pub(crate) const fn new(kind: CastErrorKind) -> Self {
        Self { kind }
    }
}
impl fmt::Display for CastError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.kind {
            CastErrorKind::Cancelled => "Receiver operation cancelled",
            CastErrorKind::Timeout => "Receiver did not respond in time",
            CastErrorKind::Network => "Cannot reach receiver",
            CastErrorKind::Protocol => "Receiver returned an invalid response",
            CastErrorKind::Unsupported => "Receiver does not support this operation",
            CastErrorKind::Takeover => "Receiver playback changed on another device",
            CastErrorKind::ReplacementRequired => {
                "Receiver is already playing media; confirmation is required"
            }
            CastErrorKind::InvalidInput => "Invalid receiver address or media request",
        })
    }
}
impl std::error::Error for CastError {}
pub(crate) type Result<T> = std::result::Result<T, CastError>;
pub(crate) fn error(kind: CastErrorKind) -> CastError {
    CastError::new(kind)
}
pub(crate) fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(error(CastErrorKind::Cancelled))
    } else {
        Ok(())
    }
}
pub(crate) async fn bounded<T>(
    cancel: &AtomicBool,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    check_cancel(cancel)?;
    tokio::pin!(future);
    let deadline = tokio::time::sleep(REQUEST_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! { result=&mut future=>return result, _=&mut deadline=>return Err(error(CastErrorKind::Timeout)), _=tokio::time::sleep(Duration::from_millis(50))=>check_cancel(cancel)? }
    }
}

/// A session owns media only after a successful LOAD acknowledgement. Connection and close
/// never launch or stop an application. There are no automatic retries of mutating requests.
pub struct ReceiverSession {
    transport: Transport,
    cancellation: Arc<AtomicBool>,
    replacement_authorized: bool,
    owned: Option<Identity>,
    takeover: bool,
}
enum Transport {
    Google(google::Google),
    Dlna(Box<dlna::Dlna>),
    Closed,
}
#[derive(Clone)]
struct Identity {
    app: Option<String>,
    media: Option<String>,
    content: String,
}
impl ReceiverSession {
    pub async fn connect(device: Device, cancellation: Arc<AtomicBool>) -> Result<Self> {
        check_cancel(&cancellation)?;
        let transport = match device.protocol {
            Protocol::GoogleCast => {
                Transport::Google(bounded(&cancellation, google::Google::connect(&device)).await?)
            }
            Protocol::Dlna => Transport::Dlna(Box::new(
                bounded(&cancellation, dlna::Dlna::connect(device)).await?,
            )),
        };
        Ok(Self {
            transport,
            cancellation,
            replacement_authorized: false,
            owned: None,
            takeover: false,
        })
    }
    pub fn needs_replacement(&self) -> bool {
        match &self.transport {
            Transport::Google(g) => g.occupied,
            Transport::Dlna(d) => d.occupied,
            Transport::Closed => false,
        }
    }
    /// Call only after explicit confirmation for the current receiver selection.
    pub fn authorize_replacement(&mut self) {
        self.replacement_authorized = true;
    }
    pub async fn load(&mut self, load: MediaLoad) -> Result<ReceiverSnapshot> {
        validate_load(&load)?;
        // Selection can remain idle while another sender starts playback. Inspect again
        // immediately before LOAD rather than relying on the connect-time busy flag.
        if self.owned.is_none() {
            match &mut self.transport {
                Transport::Google(g) => bounded(&self.cancellation, g.inspect_busy()).await?,
                Transport::Dlna(d) => bounded(&self.cancellation, d.inspect_busy()).await?,
                Transport::Closed => return Err(error(CastErrorKind::Network)),
            }
        }
        if self.needs_replacement() && self.owned.is_none() && !self.replacement_authorized {
            return Err(error(CastErrorKind::ReplacementRequired));
        }
        if self.owned.is_some() && !self.snapshot().await?.owned {
            return Err(error(CastErrorKind::Takeover));
        }
        self.replacement_authorized = false;
        let result = match &mut self.transport {
            Transport::Google(g) => bounded(&self.cancellation, g.load(&load)).await,
            Transport::Dlna(d) => bounded(&self.cancellation, d.load(&load)).await,
            Transport::Closed => return Err(error(CastErrorKind::Network)),
        };
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(original) => {
                // LOAD may have reached the receiver before its acknowledgement was
                // lost or cancelled. Observe the exact unique content URL once; never
                // replay LOAD. Retain proven ownership so cancellation cleanup can STOP.
                let cleanup = AtomicBool::new(false);
                let observed = match &mut self.transport {
                    Transport::Google(g) => bounded(&cleanup, g.snapshot()).await,
                    Transport::Dlna(d) => bounded(&cleanup, d.snapshot()).await,
                    Transport::Closed => Err(error(CastErrorKind::Network)),
                };
                let Ok(snapshot) = observed else {
                    return Err(original);
                };
                if snapshot.content_id.as_deref() != Some(&load.url) {
                    return Err(original);
                }
                self.owned = Some(Identity {
                    app: snapshot.app_session.clone(),
                    media: snapshot.media_session.clone(),
                    content: load.url.clone(),
                });
                self.takeover = false;
                if original.kind == CastErrorKind::Cancelled
                    || !matches!(
                        snapshot.state,
                        PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
                    )
                {
                    return Err(original);
                }
                snapshot
            }
        };
        // A LOAD reply must identify the requested content before ownership is granted.
        if snapshot.content_id.as_deref() != Some(&load.url) {
            return Err(error(CastErrorKind::Protocol));
        }
        self.owned = Some(Identity {
            app: snapshot.app_session.clone(),
            media: snapshot.media_session.clone(),
            content: load.url,
        });
        self.takeover = false;
        self.classify(snapshot)
    }
    pub async fn snapshot(&mut self) -> Result<ReceiverSnapshot> {
        let result = match &mut self.transport {
            Transport::Google(g) => bounded(&self.cancellation, g.snapshot()).await,
            Transport::Dlna(d) => bounded(&self.cancellation, d.snapshot()).await,
            Transport::Closed => Err(error(CastErrorKind::Network)),
        }?;
        self.classify(result)
    }
    fn classify(&mut self, mut snapshot: ReceiverSnapshot) -> Result<ReceiverSnapshot> {
        if let Some(owned) = &self.owned {
            // Terminal empty status is not a takeover; a new identity always is.
            let terminal = snapshot.state == PlaybackState::Idle
                && snapshot.content_id.is_none()
                && (snapshot.media_session.is_none() || snapshot.media_session == owned.media);
            let same = (snapshot.content_id.as_deref() == Some(&owned.content)
                && snapshot.media_session == owned.media
                || terminal)
                && snapshot.app_session == owned.app;
            if !same
                && (snapshot.content_id.is_some()
                    || snapshot.app_session != owned.app
                    || snapshot.media_session.is_some())
            {
                self.takeover = true;
            }
            snapshot.owned = same && !self.takeover;
            if self.takeover {
                snapshot.end_reason = Some(EndReason::Takeover);
            }
        }
        Ok(snapshot)
    }
    pub async fn command(&mut self, command: ReceiverCommand) -> Result<ReceiverSnapshot> {
        let before = self.snapshot().await?;
        if !before.owned {
            return Err(error(CastErrorKind::Takeover));
        }
        validate_command(command, &before.capabilities)?;
        let result = match &mut self.transport {
            Transport::Google(g) => bounded(&self.cancellation, g.command(command)).await,
            Transport::Dlna(d) => bounded(&self.cancellation, d.command(command)).await,
            Transport::Closed => Err(error(CastErrorKind::Network)),
        };
        // An uncertain acknowledgement is reconciled through observation, never replayed.
        match result {
            Ok(()) => self.snapshot().await,
            Err(e) => {
                if let Ok(after) = self.snapshot().await
                    && command_matches(command, &after)
                    && after.owned
                {
                    return Ok(after);
                }
                Err(e)
            }
        }
    }
    /// Reopen only the control connection, then verify the already-owned identity.
    /// This never launches a receiver app or issues LOAD.
    pub async fn reconnect(&mut self) -> Result<ReceiverSnapshot> {
        if self.owned.is_none() || self.takeover {
            return Err(error(CastErrorKind::Takeover));
        }
        match &mut self.transport {
            Transport::Google(g) => bounded(&self.cancellation, g.reconnect()).await?,
            Transport::Dlna(_) => {}
            Transport::Closed => return Err(error(CastErrorKind::Network)),
        }
        let snapshot = self.snapshot().await?;
        if !snapshot.owned {
            return Err(error(CastErrorKind::Takeover));
        }
        Ok(snapshot)
    }
    pub async fn stop_owned_media(&mut self) -> Result<()> {
        // Cancellation prevents new work, but teardown must still observe and stop
        // the exact media already loaded by this session. Use a fresh bounded budget.
        let cleanup = AtomicBool::new(false);
        let snapshot = match &mut self.transport {
            Transport::Google(g) => bounded(&cleanup, g.snapshot()).await,
            Transport::Dlna(d) => bounded(&cleanup, d.snapshot()).await,
            Transport::Closed => Err(error(CastErrorKind::Network)),
        }?;
        let snapshot = self.classify(snapshot)?;
        if !snapshot.owned {
            return Err(error(CastErrorKind::Takeover));
        }
        if snapshot.state == PlaybackState::Idle && snapshot.content_id.is_none() {
            self.owned = None;
            return Ok(());
        }
        let result = match &mut self.transport {
            Transport::Google(g) => bounded(&cleanup, g.stop()).await,
            Transport::Dlna(d) => bounded(&cleanup, d.stop()).await,
            Transport::Closed => Err(error(CastErrorKind::Network)),
        };
        if result.is_ok() {
            self.owned = None;
        }
        result
    }
    /// Drops transport tasks and sockets. Deliberately does not call CastClient::disconnect,
    /// because oxicast 0.0.3's disconnect sends STOP to the receiver's current media.
    pub async fn close_transport(&mut self) {
        self.transport = Transport::Closed;
    }
}
fn validate_load(load: &MediaLoad) -> Result<()> {
    if load.url.len() > 16384
        || load.title.len() > 4096
        || load.text_tracks.len() > 32
        || !load.start_position.is_finite()
        || load.start_position < 0.
        || load.duration.is_some_and(|d| !d.is_finite() || d < 0.)
        || load.mime_type.is_empty()
        || load.mime_type.chars().any(char::is_control)
    {
        return Err(error(CastErrorKind::InvalidInput));
    }
    let url = reqwest::Url::parse(&load.url).map_err(|_| error(CastErrorKind::InvalidInput))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(error(CastErrorKind::InvalidInput));
    }
    for t in &load.text_tracks {
        let u = reqwest::Url::parse(&t.url).map_err(|_| error(CastErrorKind::InvalidInput))?;
        if !matches!(u.scheme(), "http" | "https")
            || !u.username().is_empty()
            || u.password().is_some()
        {
            return Err(error(CastErrorKind::InvalidInput));
        }
    }
    Ok(())
}
fn validate_command(cmd: ReceiverCommand, c: &ReceiverCapabilities) -> Result<()> {
    let supported = match cmd {
        ReceiverCommand::Seek(p) => {
            if !p.is_finite() || p < 0. {
                return Err(error(CastErrorKind::InvalidInput));
            }
            c.seek
        }
        ReceiverCommand::SetVolume(v) => {
            if !v.is_finite() || !(0. ..=1.).contains(&v) {
                return Err(error(CastErrorKind::InvalidInput));
            }
            c.volume
        }
        ReceiverCommand::SetPaused(_) => c.pause,
        ReceiverCommand::SetMuted(_) => c.mute,
        ReceiverCommand::SetTextTrack(_) => c.text_tracks,
    };
    if supported {
        Ok(())
    } else {
        Err(error(CastErrorKind::Unsupported))
    }
}
fn command_matches(cmd: ReceiverCommand, s: &ReceiverSnapshot) -> bool {
    match cmd {
        ReceiverCommand::SetPaused(p) => {
            s.state
                == if p {
                    PlaybackState::Paused
                } else {
                    PlaybackState::Playing
                }
        }
        ReceiverCommand::Seek(p) => (s.position - p).abs() < 2.,
        ReceiverCommand::SetVolume(v) => s.volume.is_some_and(|x| (x - v).abs() < 0.02),
        ReceiverCommand::SetMuted(m) => s.muted == Some(m),
        ReceiverCommand::SetTextTrack(_) => false,
    }
}

#[cfg(test)]
mod network_tests;

//! Linux session-bus transport. D-Bus never reads Slint or calls mpv directly.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};

use zbus::blocking::{Connection, connection::Builder};
use zbus::zvariant::{ObjectPath, OwnedValue, Str, Value};

const PATH: &str = "/org/mpris/MediaPlayer2";
// The /org/mpris namespace is reserved by the specification, including NoTrack.
const TRACK_PATH: &str = "/dev/misob/nova/track";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

pub use crate::media_controls::{Command, Owner, Snapshot};

impl Snapshot {
    fn metadata(&self) -> HashMap<&'static str, OwnedValue> {
        let mut values = HashMap::new();
        if let Some(track) = self.track {
            let path = ObjectPath::try_from(format!("{TRACK_PATH}/{track}")).unwrap();
            values.insert("mpris:trackid", path.into());
            values.insert("xesam:title", Str::from(self.title.as_str()).into());
            if !self.album.is_empty() {
                values.insert("xesam:album", Str::from(self.album.as_str()).into());
            }
            if self.duration > 0 {
                values.insert("mpris:length", self.duration.into());
            }
        }
        values
    }
}

type Dispatch = Arc<dyn Fn(Option<Owner>, Command) -> Result<(), String> + Send + Sync>;

#[derive(Clone)]
struct PlayerInterface {
    state: Arc<Mutex<Snapshot>>,
    dispatch: Dispatch,
}

impl PlayerInterface {
    fn command(&self, command: Command) -> zbus::fdo::Result<()> {
        let state = self.state.lock().unwrap().clone();
        if state.track.is_none() {
            return Ok(());
        }
        (self.dispatch)(state.owner, command).map_err(zbus::fdo::Error::Failed)
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl PlayerInterface {
    fn next(&self) {}
    fn previous(&self) {}
    fn pause(&self) -> zbus::fdo::Result<()> {
        self.command(Command::Pause)
    }
    fn play_pause(&self) -> zbus::fdo::Result<()> {
        self.command(Command::TogglePause)
    }
    fn stop(&self) -> zbus::fdo::Result<()> {
        self.command(Command::Stop)
    }
    fn play(&self) -> zbus::fdo::Result<()> {
        self.command(Command::Play)
    }
    fn seek(&self, offset: i64) -> zbus::fdo::Result<()> {
        if !self.state.lock().unwrap().can_seek {
            return Ok(());
        }
        self.command(Command::Seek(offset))
    }
    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) -> zbus::fdo::Result<()> {
        let state = self.state.lock().unwrap().clone();
        let Some(track) = state.track else {
            return Ok(());
        };
        if !state.can_seek
            || position < 0
            || (state.duration > 0 && position > state.duration)
            || track_id.as_str() != format!("{TRACK_PATH}/{track}")
        {
            return Ok(());
        }
        // Carry the validated identity through to the UI, where it is checked again.
        (self.dispatch)(state.owner, Command::SetPosition(position))
            .map_err(zbus::fdo::Error::Failed)
    }
    fn open_uri(&self, _uri: &str) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported(
            "Choose a stream in Nova".into(),
        ))
    }
    #[zbus(signal)]
    async fn seeked(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        position: i64,
    ) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.state.lock().unwrap().status().into()
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<&'static str, OwnedValue> {
        self.state.lock().unwrap().metadata()
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        self.state.lock().unwrap().position
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.state.lock().unwrap().volume
    }
    #[zbus(property)]
    fn set_volume(&self, volume: f64) -> zbus::fdo::Result<()> {
        if !volume.is_finite() {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Volume must be finite".into(),
            ));
        }
        self.command(Command::SetVolume(volume.clamp(0.0, 1.0)))
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        self.state.lock().unwrap().rate
    }
    #[zbus(property)]
    fn set_rate(&self, rate: f64) -> zbus::fdo::Result<()> {
        if rate == 0.0 {
            return self.command(Command::Pause);
        }
        let state = self.state.lock().unwrap().clone();
        if !rate.is_finite() || !(state.minimum_rate..=state.maximum_rate).contains(&rate) {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Unsupported playback rate".into(),
            ));
        }
        self.command(Command::SetRate(rate))
    }
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        self.state.lock().unwrap().minimum_rate
    }
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        self.state.lock().unwrap().maximum_rate
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.state.lock().unwrap().can_play
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.state.lock().unwrap().can_play
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.state.lock().unwrap().can_seek
    }
    #[zbus(property(emits_changed_signal = "const"))]
    fn can_control(&self) -> bool {
        true
    }
}

struct RootInterface(Dispatch);

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl RootInterface {
    fn raise(&self) -> zbus::fdo::Result<()> {
        (self.0)(None, Command::Raise).map_err(zbus::fdo::Error::Failed)
    }
    fn quit(&self) {}
    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn identity(&self) -> &str {
        "Nova"
    }
    #[zbus(property)]
    fn desktop_entry(&self) -> &str {
        "nova"
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<&str> {
        Vec::new()
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<&str> {
        Vec::new()
    }
}

/// Coalesced UI snapshots; all bus setup and signal emission stay on a worker.
pub struct Service {
    state: Arc<Mutex<Snapshot>>,
    wake: mpsc::SyncSender<()>,
}

impl Service {
    pub fn start(
        dispatch: impl Fn(Option<Owner>, Command) -> Result<(), String> + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let state = Arc::new(Mutex::new(Snapshot::default()));
        let (wake, updates) = mpsc::sync_channel(1);
        let interface = PlayerInterface {
            state: state.clone(),
            dispatch: Arc::new(dispatch),
        };
        std::thread::Builder::new()
            .name("nova-mpris".into())
            .spawn(move || {
                if let Err(error) = serve(interface, updates) {
                    eprintln!("nova player: MPRIS session bus unavailable: {error}");
                }
            })?;
        Ok(Self { state, wake })
    }

    pub fn publish(&self, snapshot: Snapshot) {
        *self.state.lock().unwrap() = snapshot;
        // A queued wake reads the newest snapshot; slow bus clients cannot
        // accumulate state updates or block the rendering/event-loop thread.
        let _ = self.wake.try_send(());
    }
}

fn serve(interface: PlayerInterface, updates: mpsc::Receiver<()>) -> zbus::Result<()> {
    let connection = Builder::session()?
        .name(format!(
            "org.mpris.MediaPlayer2.nova.instance{}",
            std::process::id()
        ))?
        .serve_at(PATH, RootInterface(interface.dispatch.clone()))?
        .serve_at(PATH, interface.clone())?
        .build()?;
    let mut previous = Snapshot::default();
    while updates.recv().is_ok() {
        let current = interface.state.lock().unwrap().clone();
        publish_changes(&connection, &previous, &current)?;
        previous = current;
    }
    Ok(())
}

fn publish_changes(connection: &Connection, old: &Snapshot, new: &Snapshot) -> zbus::Result<()> {
    let mut changed: HashMap<&str, Value<'_>> = HashMap::new();
    if old.status() != new.status() {
        changed.insert("PlaybackStatus", new.status().into());
    }
    if old.track != new.track
        || old.title != new.title
        || old.album != new.album
        || old.duration != new.duration
    {
        changed.insert("Metadata", Value::from(new.metadata()));
    }
    if old.volume != new.volume {
        changed.insert("Volume", new.volume.into());
    }
    if old.rate != new.rate {
        changed.insert("Rate", new.rate.into());
    }
    if old.minimum_rate != new.minimum_rate {
        changed.insert("MinimumRate", new.minimum_rate.into());
    }
    if old.maximum_rate != new.maximum_rate {
        changed.insert("MaximumRate", new.maximum_rate.into());
    }
    if old.can_play != new.can_play {
        changed.insert("CanPlay", new.can_play.into());
        changed.insert("CanPause", new.can_play.into());
    }
    if old.can_seek != new.can_seek {
        changed.insert("CanSeek", new.can_seek.into());
    }
    if !changed.is_empty() {
        connection.emit_signal(
            None::<&str>,
            PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(PLAYER, changed, Vec::<&str>::new()),
        )?;
    }
    let sought = old.track == new.track && old.seek_serial != new.seek_serial;
    let resumed_position =
        new.position > 0 && (old.track != new.track || (!old.playing && new.playing));
    if new.track.is_some() && (sought || resumed_position) {
        connection.emit_signal(None::<&str>, PATH, PLAYER, "Seeked", &(new.position,))?;
    }
    Ok(())
}

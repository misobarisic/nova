//! Platform-neutral system controls. Metadata never contains a source URL.
//! Ownership is carried back unchanged and revalidated by the app on its UI thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub source: u64,
    pub profile_epoch: u64,
    pub room: Option<(String, u64, u64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub owner: Option<Owner>,
    pub track: Option<u64>,
    pub title: String,
    pub album: String,
    pub playing: bool,
    pub paused: bool,
    pub can_play: bool,
    pub can_seek: bool,
    pub position: i64,
    pub duration: i64,
    pub volume: f64,
    pub rate: f64,
    pub minimum_rate: f64,
    pub maximum_rate: f64,
    pub seek_serial: u64,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            owner: None,
            track: None,
            title: String::new(),
            album: String::new(),
            playing: false,
            paused: false,
            can_play: false,
            can_seek: false,
            position: 0,
            duration: 0,
            volume: 1.0,
            rate: 1.0,
            minimum_rate: f64::from(nova_config::PLAYBACK_SPEED_MIN),
            maximum_rate: f64::from(nova_config::PLAYBACK_SPEED_MAX),
            seek_serial: 0,
        }
    }
}

impl Snapshot {
    pub(crate) fn status(&self) -> &'static str {
        if !self.playing {
            "Stopped"
        } else if self.paused {
            "Paused"
        } else {
            "Playing"
        }
    }
}

#[derive(Clone, Debug)]
pub enum Command {
    Raise,
    Play,
    Pause,
    TogglePause,
    Stop,
    /// Signed microseconds, matching MPRIS. Windows converts its 100 ns ticks.
    Seek(i64),
    SetPosition(i64),
    SetVolume(f64),
    SetRate(f64),
}

#[cfg(any(target_os = "windows", test))]
pub(crate) fn accepts_native_command(
    snapshot: &Snapshot,
    owner: &Owner,
    command: &Command,
) -> bool {
    snapshot.owner.as_ref() == Some(owner)
        && snapshot.track == Some(owner.source)
        && match command {
            Command::Play | Command::Pause | Command::TogglePause | Command::SetRate(_) => {
                snapshot.can_play
            }
            Command::Seek(_) | Command::SetPosition(_) => snapshot.can_seek,
            Command::Stop => true,
            _ => false,
        }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
impl crate::Player {
    /// Includes transient native rate changes; party callers publish their
    /// accepted room target separately from clock correction.
    pub fn current_playback_speed(&self) -> f64 {
        self.state
            .mpv
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|mpv| mpv.get_property::<f64>("speed").ok())
            .filter(|rate| rate.is_finite() && *rate > 0.0)
            .unwrap_or_else(|| f64::from(self.effective_playback_speed()))
    }

    pub fn seek_serial(&self) -> u64 {
        self.state
            .seek_serial
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(any(target_os = "windows", test))]
pub(crate) fn timeline_due(old: &Snapshot, new: &Snapshot, elapsed: std::time::Duration) -> bool {
    old.owner != new.owner
        || old.status() != new.status()
        || old.seek_serial != new.seek_serial
        || old.duration != new.duration
        || old.can_seek != new.can_seek
        || old.rate != new.rate
        || (new.track.is_some() && elapsed >= std::time::Duration::from_secs(5))
}

#[cfg(any(target_os = "windows", test))]
pub(crate) fn windows_ticks(micros: i64) -> i64 {
    micros.max(0).saturating_mul(10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn old_native_callbacks_never_acquire_a_replacement_owner() {
        let published = Owner {
            source: 3,
            profile_epoch: 7,
            room: Some(("room-a".into(), 11, 2)),
        };
        let mut desired = Snapshot {
            owner: Some(published.clone()),
            track: Some(3),
            can_play: true,
            can_seek: true,
            ..Default::default()
        };
        assert!(accepts_native_command(
            &desired,
            &published,
            &Command::Pause
        ));
        for boundary in [
            "source",
            "profile",
            "room",
            "generation",
            "revision",
            "closed",
        ] {
            let mut replacement = published.clone();
            match boundary {
                "source" => replacement.source += 1,
                "profile" => replacement.profile_epoch += 1,
                "room" => replacement.room.as_mut().unwrap().0 = "room-b".into(),
                "generation" => replacement.room.as_mut().unwrap().1 += 1,
                "revision" => replacement.room.as_mut().unwrap().2 += 1,
                "closed" => {}
                _ => unreachable!(),
            }
            desired.owner = (boundary != "closed").then_some(replacement);
            desired.track = desired.owner.as_ref().map(|owner| owner.source);
            for command in [
                Command::Play,
                Command::Stop,
                Command::SetPosition(100),
                Command::SetRate(1.5),
            ] {
                assert!(
                    !accepts_native_command(&desired, &published, &command),
                    "{boundary}: an old registration cannot borrow desired ownership"
                );
            }
        }
        desired.owner = Some(published.clone());
        desired.track = Some(published.source);
        desired.can_play = false;
        desired.can_seek = false;
        assert!(!accepts_native_command(
            &desired,
            &published,
            &Command::Play
        ));
        assert!(!accepts_native_command(
            &desired,
            &published,
            &Command::SetPosition(100)
        ));
        assert!(
            accepts_native_command(&desired, &published, &Command::Stop),
            "Read-only party members can still close their own source"
        );
    }

    #[test]
    fn windows_timeline_refreshes_on_transport_changes_or_five_seconds() {
        let mut old = Snapshot {
            track: Some(1),
            playing: true,
            ..Default::default()
        };
        let mut current = old.clone();
        current.position = 250_000;
        assert!(!timeline_due(&old, &current, Duration::from_millis(250)));
        assert!(timeline_due(&old, &current, Duration::from_secs(5)));
        current.paused = true;
        assert!(timeline_due(&old, &current, Duration::ZERO));
        old = current.clone();
        current.seek_serial += 1;
        assert!(timeline_due(&old, &current, Duration::ZERO));
        old = current.clone();
        current.rate = 1.5;
        assert!(timeline_due(&old, &current, Duration::ZERO));
        assert_eq!(windows_ticks(1_250_000), 12_500_000);
        assert_eq!(windows_ticks(-1), 0);
        assert_eq!(windows_ticks(i64::MAX), i64::MAX);
    }
}

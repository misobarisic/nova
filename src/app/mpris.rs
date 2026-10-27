//! Linux media controls use the same guarded UI-thread actions as Nova's OSD.
use super::*;
use nova_playback_api::PlaybackAction;
use nova_player::mpris::{Command, Service, Snapshot};

pub(super) fn install(bridge: &Bridge) -> Option<Service> {
    let bridge = bridge.clone();
    match Service::start(move |track, command| {
        let bridge = bridge.clone();
        slint::invoke_from_event_loop(move || bridge.handle_mpris(track, command))
            .map_err(|error| error.to_string())
    }) {
        Ok(service) => Some(service),
        Err(error) => {
            tracing::warn!(%error, "Could not start Linux media controls");
            None
        }
    }
}

fn micros(seconds: f32) -> i64 {
    if seconds.is_finite() {
        (f64::from(seconds.max(0.0)) * 1_000_000.0) as i64
    } else {
        0
    }
}

impl Bridge {
    pub(super) fn mpris_snapshot(&self) -> Snapshot {
        let Some(app) = self.app() else {
            return Snapshot::default();
        };
        if !app.get_player_open()
            || app.get_profiles_require_selection()
            || !self
                .shared
                .lock()
                .unwrap()
                .remote_playback
                .can_play_locally()
        {
            return Snapshot::default();
        }
        let party = self.party_active();
        let ready = app.get_playback_started()
            && !self.player.playback_failed()
            && !self.player.awaiting_start_confirmation();
        let timeline = ready && (!party || app.get_party_is_host());
        let episode = app.get_player_episode_title().to_string();
        let show = app.get_player_show_title().to_string();
        let readiness = self.player.playback_readiness();
        Snapshot {
            track: Some(self.player.screenshot_session()),
            title: if episode.is_empty() {
                show.clone()
            } else {
                episode
            },
            album: show,
            playing: ready && !readiness.ended,
            paused: app.get_is_paused(),
            can_play: timeline,
            can_seek: timeline && readiness.seekable,
            position: micros(app.get_position()),
            duration: micros(app.get_duration()),
            volume: if app.get_muted() {
                0.0
            } else {
                f64::from(app.get_volume()) / 100.0
            },
            // Party clock corrections are temporary synchronization nudges;
            // its user-visible playback rate stays at normal speed.
            rate: if party {
                1.0
            } else {
                self.player.current_playback_speed()
            },
            minimum_rate: if party {
                1.0
            } else {
                f64::from(nova_config::PLAYBACK_SPEED_MIN)
            },
            maximum_rate: if party {
                1.0
            } else {
                f64::from(nova_config::PLAYBACK_SPEED_MAX)
            },
            seek_serial: self.player.seek_serial(),
        }
    }

    fn handle_mpris(&self, track: Option<u64>, command: Command) {
        if matches!(command, Command::Raise) {
            if let Some(app) = self.app() {
                let _ = app.show();
                use slint::winit_030::WinitWindowAccessor;
                app.window().with_winit_window(|window| {
                    window.set_minimized(false);
                    window.focus_window();
                });
            }
            return;
        }
        let state = self.mpris_snapshot();
        // A key or slider message queued before a source/profile change may
        // never control its replacement, even when its callback arrives late.
        if track.is_none() || track != state.track {
            return;
        }
        let action = match command {
            Command::Play if state.can_play => PlaybackAction::SetPaused(false),
            Command::Pause if state.can_play => PlaybackAction::SetPaused(true),
            Command::TogglePause if state.can_play => PlaybackAction::TogglePause,
            Command::Stop => PlaybackAction::Stop,
            Command::Seek(offset) if state.can_seek => {
                // With no MPRIS next track, seeking beyond the end has the
                // same no-op behavior as Next (rather than closing playback).
                if state.duration > 0 && state.position.saturating_add(offset) > state.duration {
                    return;
                }
                PlaybackAction::SeekRelative(offset as f64 / 1_000_000.0)
            }
            Command::SetPosition(position)
                if state.can_seek
                    && position >= 0
                    && (state.duration <= 0 || position <= state.duration) =>
            {
                PlaybackAction::SeekAbsolute(position as f64 / 1_000_000.0)
            }
            Command::SetVolume(volume) => PlaybackAction::SetVolume(volume * 100.0),
            Command::SetRate(rate)
                if state.can_play && (state.minimum_rate..=state.maximum_rate).contains(&rate) =>
            {
                PlaybackAction::SetSpeed(rate)
            }
            _ => return,
        };
        if let Err(error) = self.dispatch_playback_action(action) {
            tracing::debug!(%error, "Linux media control was declined");
        }
    }
}

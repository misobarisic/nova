//! Shared Linux MPRIS / Windows SMTC adapter. Only the UI thread reads app/player
//! state or executes actions; native workers receive metadata and opaque owners.
use super::*;
use nova_playback_api::PlaybackAction;
use nova_player::media_controls::{Command, Owner, Snapshot};
#[cfg(target_os = "linux")]
use nova_player::mpris::Service as NativeService;
#[cfg(target_os = "windows")]
use nova_player::smtc::Service as NativeService;

pub(super) struct Service {
    native: Option<NativeService>,
    #[cfg(target_os = "windows")]
    attempted: bool,
}

fn dispatch(
    bridge: &Bridge,
) -> impl Fn(Option<Owner>, Command) -> Result<(), String> + Send + Sync + 'static {
    let bridge = bridge.clone();
    move |owner, command| {
        let bridge = bridge.clone();
        slint::invoke_from_event_loop(move || bridge.handle_system_media(owner, command))
            .map_err(|error| error.to_string())
    }
}

impl Service {
    pub(super) fn new(bridge: &Bridge) -> Self {
        #[cfg(target_os = "linux")]
        {
            Self {
                native: NativeService::start(dispatch(bridge))
                    .map_err(|error| tracing::warn!(%error, "Could not start Linux media controls"))
                    .ok(),
            }
        }
        #[cfg(target_os = "windows")]
        {
            let _ = bridge;
            Self {
                native: None,
                attempted: false,
            }
        }
    }

    pub(super) fn publish(&mut self, bridge: &Bridge) {
        #[cfg(target_os = "windows")]
        if !self.attempted {
            // Winit creates the native window only once the event loop starts.
            // Copy the HWND on the UI thread; COM setup runs on SMTC's worker.
            use slint::winit_030::WinitWindowAccessor;
            use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let hwnd = bridge.app().and_then(|app| {
                app.window()
                    .with_winit_window(|window| match window.window_handle().ok()?.as_raw() {
                        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
                        _ => None,
                    })
                    .flatten()
            });
            if let Some(hwnd) = hwnd {
                self.attempted = true;
                self.native = NativeService::start(hwnd, dispatch(bridge))
                    .map_err(
                        |error| tracing::warn!(%error, "Could not start Windows media controls"),
                    )
                    .ok();
            }
        }
        if let Some(native) = &self.native {
            native.publish(bridge.system_media_snapshot());
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

fn action(state: &Snapshot, command: Command) -> Option<PlaybackAction> {
    Some(match command {
        Command::Play if state.can_play => PlaybackAction::SetPaused(false),
        Command::Pause if state.can_play => PlaybackAction::SetPaused(true),
        Command::TogglePause if state.can_play => PlaybackAction::TogglePause,
        // Stop closes this device's player. The party coordinator retains its
        // existing creator/guest semantics; it never grants guest room admin.
        Command::Stop => PlaybackAction::Stop,
        Command::Seek(offset) if state.can_seek => {
            if state.duration > 0 && state.position.saturating_add(offset) > state.duration {
                return None;
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
        Command::SetVolume(volume) if volume.is_finite() && (0.0..=1.0).contains(&volume) => {
            PlaybackAction::SetVolume(volume * 100.0)
        }
        Command::SetRate(rate)
            if state.can_play
                && rate.is_finite()
                && (state.minimum_rate..=state.maximum_rate).contains(&rate) =>
        {
            PlaybackAction::SetSpeed(rate)
        }
        _ => return None,
    })
}

impl Bridge {
    pub(super) fn system_media_snapshot(&self) -> Snapshot {
        let Some(app) = self.app() else {
            return Snapshot::default();
        };
        let (local, room, party_bound) = {
            let state = self.shared.lock().unwrap();
            (
                state.remote_playback.can_play_locally()
                    && !state.casting.active
                    && !state.casting.loading,
                state.party.snapshot_for_sources(),
                state.playback_intent.as_ref().is_some_and(|intent| {
                    intent.active_local() && intent.party_bound_to(&state.party)
                }),
            )
        };
        if !app.get_player_open()
            || app.get_profiles_require_selection()
            || profile_policy::entry_blocked()
            || !local
        {
            return Snapshot::default();
        }
        let party = self.party_active();
        if party && (room.is_none() || !party_bound) {
            return Snapshot::default();
        }
        let ready = app.get_playback_started()
            && !self.player.playback_failed()
            && !self.player.awaiting_start_confirmation();
        let permission = room.as_ref().is_some_and(|room| {
            room.connected
                && !room.ended
                && (room.role == nova_party::PartyRole::Host
                    || room.control_policy == nova_party::ControlPolicy::Everyone)
        });
        let timeline = ready && (!party || (app.get_party_can_control() && permission));
        let episode = app.get_player_episode_title().to_string();
        let show = app.get_player_show_title().to_string();
        let readiness = self.player.playback_readiness();
        let source = self.player.screenshot_session();
        Snapshot {
            owner: Some(Owner {
                source,
                profile_epoch: profiles::capture().epoch,
                room: party
                    .then(|| {
                        room.as_ref().map(|room| {
                            (room.room_id.clone(), room.generation, room.control_revision)
                        })
                    })
                    .flatten(),
            }),
            track: Some(source),
            title: if episode.is_empty() {
                show.clone()
            } else {
                episode
            },
            album: show,
            playing: ready && !readiness.ended,
            // The native decoder may still be paused for a coordinated start.
            // OS widgets follow the same pending/logical intent as Nova's OSD.
            paused: if party {
                app.get_party_paused()
            } else {
                app.get_is_paused()
            },
            can_play: timeline,
            can_seek: timeline && readiness.seekable,
            position: micros(app.get_position()),
            duration: micros(app.get_duration()),
            volume: if app.get_muted() {
                0.0
            } else {
                f64::from(app.get_volume()) / 100.0
            },
            rate: if party {
                room.as_ref().unwrap().timeline.target_rate
            } else {
                self.player.current_playback_speed()
            },
            minimum_rate: f64::from(nova_config::PLAYBACK_SPEED_MIN),
            maximum_rate: f64::from(nova_config::PLAYBACK_SPEED_MAX),
            seek_serial: self.player.seek_serial(),
        }
    }

    pub(super) fn handle_system_media(&self, owner: Option<Owner>, command: Command) {
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
        let state = self.system_media_snapshot();
        // Commands posted before source/profile/room/policy replacement can
        // never acquire authority over its replacement on UI delivery.
        if owner.is_none() || owner != state.owner {
            return;
        }
        let Some(action) = action(&state, command) else {
            return;
        };
        if let Err(error) = self.dispatch_playback_action(action) {
            tracing::debug!(%error, "System media control was declined");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_requests_validate_seek_and_rate_capabilities() {
        let mut state = Snapshot {
            can_play: true,
            can_seek: true,
            position: 30_000_000,
            duration: 120_000_000,
            ..Default::default()
        };
        assert!(
            matches!(action(&state, Command::SetPosition(45_500_000)), Some(PlaybackAction::SeekAbsolute(position)) if position == 45.5)
        );
        assert!(action(&state, Command::SetPosition(-1)).is_none());
        assert!(action(&state, Command::SetPosition(121_000_000)).is_none());
        assert!(action(&state, Command::Seek(100_000_000)).is_none());
        for rate in [f64::NAN, f64::INFINITY, 0.0, 3.0] {
            assert!(action(&state, Command::SetRate(rate)).is_none());
        }
        assert!(
            matches!(action(&state, Command::SetRate(1.55)), Some(PlaybackAction::SetSpeed(rate)) if rate == 1.55)
        );
        state.can_play = false;
        state.can_seek = false;
        for command in [
            Command::Play,
            Command::Pause,
            Command::TogglePause,
            Command::SetRate(1.5),
            Command::SetPosition(10_000_000),
        ] {
            assert!(action(&state, command).is_none());
        }
        assert!(matches!(
            action(&state, Command::Stop),
            Some(PlaybackAction::Stop)
        ));
    }

    #[test]
    fn system_media_dispatch_rechecks_source_profile_and_local_output_before_delivery() {
        const ROOT: &str = "NOVA_SYSTEM_MEDIA_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-system-media-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap()).env(ROOT, &root)
                .args(["--exact", "app::media_controls::tests::system_media_dispatch_rechecks_source_profile_and_local_output_before_delivery", "--nocapture"])
                .output().unwrap();
            let _ = fs::remove_dir_all(root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        storage::init_at(&root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        let app = AppWindow::new().unwrap();
        let player = crate::player::Player::setup(&app);
        let (hi, _) = mpsc::channel();
        let (lo, _) = mpsc::channel();
        let bridge = Bridge::new(
            app.as_weak(),
            PosterTx { hi, lo },
            Arc::new(AtomicU64::new(0)),
            player,
            DownloadCoordinator::new(root.join("downloads")),
        );
        app.set_player_open(true);
        app.set_playback_started(true);
        app.set_player_show_title("Example series".into());
        app.set_player_episode_title("Episode two".into());
        app.set_position(32.0);
        app.set_duration(120.0);
        let initial = bridge.system_media_snapshot();
        assert_eq!(initial.title, "Episode two");
        assert_eq!(initial.album, "Example series");
        assert_eq!(initial.position, 32_000_000);
        assert_eq!(initial.duration, 120_000_000);
        let mut stale_source = initial.owner.clone().unwrap();
        stale_source.source = stale_source.source.wrapping_add(1);
        bridge.handle_system_media(Some(stale_source), Command::Stop);
        assert!(app.get_player_open());
        let mut context = profiles::capture();
        context.epoch += 1;
        profiles::with_context(context, || {
            bridge.handle_system_media(initial.owner.clone(), Command::Stop);
            assert!(
                app.get_player_open(),
                "A delayed old-profile stop cannot close replacement playback"
            );
        });
        for blocked in ["profile", "cast", "cast-loading", "selection"] {
            match blocked {
                "profile" => profile_policy::set_entry_blocked(true),
                "cast" => bridge.shared.lock().unwrap().casting.active = true,
                "cast-loading" => bridge.shared.lock().unwrap().casting.loading = true,
                "selection" => app.set_profiles_require_selection(true),
                _ => unreachable!(),
            }
            let hidden = bridge.system_media_snapshot();
            assert!(hidden.owner.is_none());
            assert!(hidden.track.is_none());
            assert!(hidden.title.is_empty());
            bridge.handle_system_media(initial.owner.clone(), Command::Stop);
            assert!(
                app.get_player_open(),
                "{blocked}: late control cannot cross ownership boundary"
            );
            profile_policy::set_entry_blocked(false);
            bridge.shared.lock().unwrap().casting.active = false;
            bridge.shared.lock().unwrap().casting.loading = false;
            app.set_profiles_require_selection(false);
        }
        bridge.handle_system_media(initial.owner, Command::Stop);
        assert!(
            !app.get_player_open(),
            "A current local owner uses the ordinary Stop action"
        );
        assert_eq!(bridge.system_media_snapshot(), Snapshot::default());
    }
}

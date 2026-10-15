//! One UI-thread command path for local controls and authorized remote commands.
use super::*;
use nova_playback_api::PlaybackAction;

impl Bridge {
    pub(super) fn dispatch_playback_action(&self, action: PlaybackAction) -> Result<(), String> {
        self.dispatch_playback_action_from(action, None)
    }

    pub(super) fn dispatch_playback_action_from(
        &self,
        action: PlaybackAction,
        peer: Option<&nova_playback_api::PeerContext>,
    ) -> Result<(), String> {
        if !self.prepare_speed_preview(&action, peer)? {
            return Ok(());
        }
        self.execute_playback_action(action)
    }

    pub(super) fn execute_playback_action(&self, action: PlaybackAction) -> Result<(), String> {
        let app = self
            .app()
            .ok_or_else(|| "Playback window is unavailable".to_owned())?;
        if !self
            .shared
            .lock()
            .unwrap()
            .remote_playback
            .can_play_locally()
        {
            return Err("This device is controlling remote playback".into());
        }
        if !app.get_player_open()
            && !matches!(
                action,
                PlaybackAction::Resume(_)
                    | PlaybackAction::CancelResume
                    | PlaybackAction::Stop
                    | PlaybackAction::EndSpeedPreview
            )
        {
            return Err("No active playback".into());
        }
        match action {
            PlaybackAction::TogglePause => self.change_playback_pause(!self.player.is_paused()),
            PlaybackAction::SetPaused(paused) => self.change_playback_pause(paused),
            PlaybackAction::SeekRelative(delta) => {
                finite(delta)?;
                self.player.seek_relative(delta);
                self.refresh_next_episode_prompt();
            }
            PlaybackAction::SeekAbsolute(position) => {
                finite(position)?;
                let duration = app.get_duration() as f64;
                let position = if duration > 0.0 {
                    position.clamp(0.0, duration)
                } else {
                    position.max(0.0)
                };
                self.player.seek(position.min(f32::MAX as f64) as f32);
                self.refresh_next_episode_prompt();
            }
            PlaybackAction::SetVolume(volume) => {
                finite(volume)?;
                #[cfg(target_os = "android")]
                {
                    android_player::player_volume_set(volume as f32);
                    self.player.set_muted(false);
                }
                #[cfg(not(target_os = "android"))]
                self.player.set_volume(volume as f32);
            }
            PlaybackAction::VolumeStep(delta) => {
                finite(delta)?;
                #[cfg(target_os = "android")]
                {
                    android_player::player_volume_set(
                        android_player::player_volume() + delta as f32,
                    );
                    self.player.set_muted(false);
                }
                #[cfg(not(target_os = "android"))]
                self.player.step_volume(delta);
            }
            PlaybackAction::SystemVolumeStep(direction) => {
                #[cfg(target_os = "android")]
                self.android_volume_step(direction);
                #[cfg(not(target_os = "android"))]
                {
                    let _ = direction;
                    return Err("System volume is available only on Android".into());
                }
            }
            PlaybackAction::ToggleMute => self.player.toggle_mute(),
            PlaybackAction::SetMuted(muted) => self.player.set_muted(muted),
            PlaybackAction::SetSpeed(speed) => {
                finite(speed)?;
                self.apply_playback_speed(speed as f32);
            }
            PlaybackAction::SpeedStep(direction) => {
                let speed = nova_config::quantize_playback_speed(
                    self.player.effective_playback_speed()
                        + direction as f32 * nova_config::PLAYBACK_SPEED_STEP,
                );
                self.apply_playback_speed(speed);
            }
            PlaybackAction::BeginSpeedPreview => self.preview_playback_speed(2.0),
            PlaybackAction::EndSpeedPreview => {
                // Resolve at release time: an edit during the hold must never
                // be overwritten by the press-time UI value.
                self.preview_playback_speed(self.player.effective_playback_speed());
            }
            PlaybackAction::SelectAudioTrack(id) => self.pick_playback_track(true, id),
            PlaybackAction::SelectSubtitleTrack(id) => self.pick_playback_track(false, id),
            PlaybackAction::ResetAudioTrack => self.restore_playback_default(0),
            PlaybackAction::ResetSubtitleTrack => self.restore_playback_default(1),
            PlaybackAction::ResetSpeed => self.restore_playback_default(2),
            PlaybackAction::ToggleFullscreen => self.player.toggle_fullscreen(),
            PlaybackAction::SetFullscreen(fullscreen) => self.player.set_fullscreen(fullscreen),
            PlaybackAction::Screenshot => {
                if !app.get_playback_started() || app.get_screenshot_busy() {
                    return Err("Screenshot capture is unavailable".into());
                }
                self.take_player_screenshot();
            }
            PlaybackAction::Stop => {
                let pending = self.shared.lock().unwrap().playback_intent.is_some();
                if app.get_player_open() || app.get_resume_prompt_visible() || pending {
                    self.note_player_progress_from_ui();
                    self.player.close();
                    self.complete_playback_close();
                }
            }
            PlaybackAction::Resume(resume) => self.resume_prompt_choice(resume),
            PlaybackAction::CancelResume => self.cancel_resume_prompt(),
            PlaybackAction::NextEpisode { token } => self.choose_next_episode_streams(&token),
            PlaybackAction::DismissNextEpisode { token } => self.dismiss_next_episode(&token),
            PlaybackAction::SetDecoder(index) => {
                if !(0..=2).contains(&index) {
                    return Err("Invalid decoder".into());
                }
                #[cfg(target_os = "android")]
                self.player.pick_decoder(index);
                #[cfg(not(target_os = "android"))]
                return Err("Decoder selection is available only on Android".into());
            }
            PlaybackAction::SetBrightness(level) => {
                finite(level)?;
                #[cfg(target_os = "android")]
                app.set_player_brightness(android_player::player_brightness_set(level as f32));
                #[cfg(not(target_os = "android"))]
                return Err("Brightness is available only on Android".into());
            }
            PlaybackAction::BrightnessStep(delta) => {
                finite(delta)?;
                #[cfg(target_os = "android")]
                app.set_player_brightness(android_player::player_brightness_delta(delta as f32));
                #[cfg(not(target_os = "android"))]
                return Err("Brightness is available only on Android".into());
            }
        }
        Ok(())
    }

    fn change_playback_pause(&self, paused: bool) {
        self.player.set_paused(paused);
        if paused {
            // Revoke before asynchronous publication can renew prefetch leases.
            let mut state = self.shared.lock().unwrap();
            state.stream_prefetch_live_session = None;
            if let Some(playing) = state.playback.as_mut() {
                playing.next_episode_prefetched = None;
            }
        } else {
            self.refresh_next_episode_prompt();
        }
        self.note_player_progress_from_ui();
    }

    /// Native close and explicit Stop converge here once per opening.
    pub(super) fn complete_playback_close(&self) {
        #[cfg(target_os = "android")]
        android_player::player_brightness_reset();
        self.finish_playback_request();
        self.refresh_torrent_disk_usage();
        self.clear_next_episode_prompt();
        self.note_player_progress_from_ui();
        self.restore_detail_after_playback();
    }
}

fn finite(value: f64) -> Result<(), String> {
    if value.is_finite() {
        Ok(())
    } else {
        Err("Playback value must be finite".into())
    }
}

#[cfg(all(test, feature = "desktop"))]
mod tests {
    use super::*;

    #[test]
    fn stop_cancels_pending_requests_before_the_overlay_opens() {
        const ROOT: &str = "NOVA_PENDING_STOP_TEST_ROOT";
        let Some(root) = std::env::var_os(ROOT) else {
            let root = std::env::temp_dir().join(format!(
                "nova-pending-stop-{}-{}",
                std::process::id(),
                nova_config::now_ms()
            ));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .env(ROOT, &root)
                .args(["--exact", "app::playback_actions::tests::stop_cancels_pending_requests_before_the_overlay_opens", "--nocapture"])
                .output().unwrap();
            let _ = std::fs::remove_dir_all(&root);
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        i_slint_backend_testing::init_integration_test_with_mock_time();
        storage::init_at(&root);
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
        let pending = bridge.begin_playback_request();
        assert!(pending.is_current());
        assert!(!app.get_player_open());
        assert!(!app.get_resume_prompt_visible());
        bridge
            .dispatch_playback_action(PlaybackAction::Stop)
            .unwrap();
        assert!(
            !pending.is_current(),
            "Stop must cancel a resolver/torrent before an overlay exists"
        );
        assert!(bridge.shared.lock().unwrap().playback_intent.is_none());
        bridge
            .dispatch_playback_action(PlaybackAction::Stop)
            .unwrap();
        assert!(
            bridge.shared.lock().unwrap().playback_intent.is_none(),
            "Repeated Stop keeps the closed session idle"
        );

        app.set_player_open(true);
        app.set_position(42.0);
        for action in [
            PlaybackAction::SeekAbsolute(f64::NAN),
            PlaybackAction::SeekRelative(f64::INFINITY),
            PlaybackAction::SetSpeed(f64::NEG_INFINITY),
        ] {
            assert!(bridge.dispatch_playback_action(action).is_err());
        }
        assert_eq!(
            app.get_position(),
            42.0,
            "Malformed commands cannot mutate receiver state"
        );
    }
}

//! HWND-backed Windows system media controls. This worker owns its apartment,
//! event registrations and native updates; callbacks never touch Slint or mpv.
use crate::media_controls::{
    Command, Owner, Snapshot, accepts_native_command, timeline_due, windows_ticks,
};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
    PlaybackRateChangeRequestedEventArgs, SystemMediaTransportControls,
    SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
    SystemMediaTransportControlsTimelineProperties,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::{
    ISystemMediaTransportControlsInterop, RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize,
};

type Dispatch = Arc<dyn Fn(Option<Owner>, Command) -> Result<(), String> + Send + Sync>;

pub struct Service {
    state: Arc<Mutex<Snapshot>>,
    wake: mpsc::SyncSender<()>,
}

impl Service {
    /// The UI copies only its top-level raw HWND. It retains the window for
    /// this service's lifetime; no COM object or borrowed window crosses threads.
    pub fn start(
        hwnd: isize,
        dispatch: impl Fn(Option<Owner>, Command) -> Result<(), String> + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let state = Arc::new(Mutex::new(Snapshot::default()));
        let (wake, updates) = mpsc::sync_channel(1);
        let worker_state = state.clone();
        let dispatch: Dispatch = Arc::new(dispatch);
        std::thread::Builder::new()
            .name("nova-smtc".into())
            .spawn(move || {
                let result = (|| -> windows::core::Result<()> {
                    let _apartment = Apartment::new()?;
                    let mut controls = Native::new(hwnd)?;
                    let mut previous = Snapshot::default();
                    let mut last_timeline = Instant::now();
                    while updates.recv().is_ok() {
                        let current = worker_state.lock().unwrap().clone();
                        let rebind = previous.owner != current.owner;
                        if rebind {
                            controls.unregister();
                        }
                        let timeline = timeline_due(&previous, &current, last_timeline.elapsed());
                        controls.publish(&previous, &current, timeline)?;
                        if rebind && let Some(owner) = &current.owner {
                            // Register only after publishing this owner's native
                            // state. Old/in-flight handlers retain their owner.
                            controls.register(
                                owner.clone(),
                                worker_state.clone(),
                                dispatch.clone(),
                            )?;
                        }
                        if timeline {
                            last_timeline = Instant::now();
                        }
                        previous = current;
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    eprintln!("nova player: Windows system media controls unavailable: {error}");
                }
                *worker_state.lock().unwrap() = Snapshot::default();
            })?;
        Ok(Self { state, wake })
    }

    pub fn publish(&self, snapshot: Snapshot) {
        *self.state.lock().unwrap() = snapshot;
        let _ = self.wake.try_send(());
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        // Revoke callback ownership immediately; closing the channel lets the
        // worker unregister callbacks and release its apartment without a UI join.
        *self.state.lock().unwrap() = Snapshot::default();
    }
}

struct Apartment;
impl Apartment {
    fn new() -> windows::core::Result<Self> {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}

struct Native {
    controls: SystemMediaTransportControls,
    button: Option<i64>,
    position: Option<i64>,
    rate: Option<i64>,
}

fn dispatch(state: &Mutex<Snapshot>, send: &Dispatch, owner: &Owner, command: Command) {
    let snapshot = state.lock().unwrap().clone();
    if accepts_native_command(&snapshot, owner, &command) {
        let _ = send(Some(owner.clone()), command);
    }
}

impl Native {
    fn new(hwnd: isize) -> windows::core::Result<Self> {
        let interop = windows::core::factory::<
            SystemMediaTransportControls,
            ISystemMediaTransportControlsInterop,
        >()?;
        // GetForCurrentView is for CoreWindow/UWP and fails in this desktop app.
        let controls: SystemMediaTransportControls =
            unsafe { interop.GetForWindow(HWND(hwnd as *mut _))? };
        let native = Self {
            controls,
            button: None,
            position: None,
            rate: None,
        };
        native.controls.SetIsEnabled(false)?;
        native.controls.SetIsPlayEnabled(false)?;
        native.controls.SetIsPauseEnabled(false)?;
        native.controls.SetIsStopEnabled(false)?;
        native
            .controls
            .SetPlaybackStatus(MediaPlaybackStatus::Closed)?;
        native.controls.SetPlaybackRate(1.0)?;
        native.controls.SetIsNextEnabled(false)?;
        native.controls.SetIsPreviousEnabled(false)?;
        native.controls.SetIsFastForwardEnabled(false)?;
        native.controls.SetIsRewindEnabled(false)?;
        native.controls.SetIsChannelUpEnabled(false)?;
        native.controls.SetIsChannelDownEnabled(false)?;
        Ok(native)
    }

    fn register(
        &mut self,
        owner: Owner,
        state: Arc<Mutex<Snapshot>>,
        send: Dispatch,
    ) -> windows::core::Result<()> {
        self.button = Some(self.controls.ButtonPressed(&TypedEventHandler::<
            SystemMediaTransportControls,
            SystemMediaTransportControlsButtonPressedEventArgs,
        >::new({
            let state = state.clone();
            let send = send.clone();
            let owner = owner.clone();
            move |_, args| {
                if let Some(args) = args.as_ref() {
                    let command = match args.Button()? {
                        SystemMediaTransportControlsButton::Play => Some(Command::Play),
                        SystemMediaTransportControlsButton::Pause => Some(Command::Pause),
                        SystemMediaTransportControlsButton::Stop => Some(Command::Stop),
                        _ => None,
                    };
                    if let Some(command) = command {
                        dispatch(&state, &send, &owner, command);
                    }
                }
                Ok(())
            }
        }))?);
        self.position = Some(self.controls.PlaybackPositionChangeRequested(
            &TypedEventHandler::<
                SystemMediaTransportControls,
                PlaybackPositionChangeRequestedEventArgs,
            >::new({
                let state = state.clone();
                let send = send.clone();
                let owner = owner.clone();
                move |_, args| {
                    if let Some(args) = args.as_ref() {
                        let ticks = args.RequestedPlaybackPosition()?.Duration;
                        // Reject negative sub-microsecond values before division.
                        if ticks >= 0 {
                            dispatch(&state, &send, &owner, Command::SetPosition(ticks / 10));
                        }
                    }
                    Ok(())
                }
            }),
        )?);
        self.rate = Some(
            self.controls
                .PlaybackRateChangeRequested(&TypedEventHandler::<
                    SystemMediaTransportControls,
                    PlaybackRateChangeRequestedEventArgs,
                >::new(move |_, args| {
                    if let Some(args) = args.as_ref() {
                        dispatch(
                            &state,
                            &send,
                            &owner,
                            Command::SetRate(args.RequestedPlaybackRate()?),
                        );
                    }
                    Ok(())
                }))?,
        );
        Ok(())
    }

    fn unregister(&mut self) {
        if let Some(token) = self.button.take() {
            let _ = self.controls.RemoveButtonPressed(token);
        }
        if let Some(token) = self.position.take() {
            let _ = self.controls.RemovePlaybackPositionChangeRequested(token);
        }
        if let Some(token) = self.rate.take() {
            let _ = self.controls.RemovePlaybackRateChangeRequested(token);
        }
    }

    fn publish(&self, old: &Snapshot, new: &Snapshot, timeline: bool) -> windows::core::Result<()> {
        let controls = &self.controls;
        if old.track != new.track || old.title != new.title || old.album != new.album {
            let updater = controls.DisplayUpdater()?;
            updater.ClearAll()?;
            if new.track.is_some() {
                updater.SetType(MediaPlaybackType::Video)?;
                updater
                    .VideoProperties()?
                    .SetTitle(&new.title.as_str().into())?;
                updater
                    .VideoProperties()?
                    .SetSubtitle(&new.album.as_str().into())?;
            }
            updater.Update()?;
        }
        if old.track != new.track {
            controls.SetIsEnabled(new.track.is_some())?;
        }
        if old.can_play != new.can_play {
            controls.SetIsPlayEnabled(new.can_play)?;
            controls.SetIsPauseEnabled(new.can_play)?;
        }
        if old.track != new.track {
            controls.SetIsStopEnabled(new.track.is_some())?;
        }
        if old.track != new.track || old.status() != new.status() {
            controls.SetPlaybackStatus(if !new.playing {
                MediaPlaybackStatus::Stopped
            } else if new.paused {
                MediaPlaybackStatus::Paused
            } else {
                MediaPlaybackStatus::Playing
            })?;
        }
        if old.rate != new.rate {
            controls.SetPlaybackRate(new.rate)?;
        }
        if timeline {
            let properties = SystemMediaTransportControlsTimelineProperties::new()?;
            let position = windows_ticks(new.position);
            let duration = windows_ticks(new.duration).max(position);
            properties.SetStartTime(TimeSpan { Duration: 0 })?;
            properties.SetEndTime(TimeSpan { Duration: duration })?;
            properties.SetPosition(TimeSpan { Duration: position })?;
            properties.SetMinSeekTime(TimeSpan {
                Duration: if new.can_seek { 0 } else { position },
            })?;
            properties.SetMaxSeekTime(TimeSpan {
                Duration: if new.can_seek { duration } else { position },
            })?;
            controls.UpdateTimelineProperties(&properties)?;
        }
        Ok(())
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        self.unregister();
        let _ = self.controls.SetPlaybackStatus(MediaPlaybackStatus::Closed);
        if let Ok(updater) = self.controls.DisplayUpdater() {
            let _ = updater.ClearAll();
            let _ = updater.Update();
        }
        let _ = self.controls.SetIsEnabled(false);
    }
}

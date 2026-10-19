//! Trakt device authorization, atomic refresh rotation and real player events.
use super::*;
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct Refresh {
    token: Secret,
    expires_at: u64,
}
pub(super) struct DeviceSession {
    client_id: String,
    code: nova_list_services::DeviceCode,
    expires: Instant,
    next: Instant,
    interval: u64,
    generation: u64,
    _background: Box<dyn Send>,
}
#[derive(Clone)]
pub(super) struct Observation {
    pub(super) series_id: String,
    pub(super) episode_id: String,
    pub(super) action: ScrobbleAction,
    percent: f64,
    session: u64,
    native: u64,
    replay: u64,
    at: u64,
}
/// The retained EOF frame outlives its scrobble. Only a new native load or
/// actual seek/replay rearms it; a later pause tick or close cannot stop twice.
#[derive(Default)]
pub(super) struct PlaybackEnd {
    owner: Option<(String, String, u64, u64)>,
    stopped_at: Option<f64>,
    replay: u64,
}
impl PlaybackEnd {
    fn observe(
        &mut self,
        target: &PlaybackTarget,
        native: u64,
        position: f64,
        duration: f64,
        paused: bool,
        stopped: bool,
    ) -> Option<u64> {
        // Player::close invalidates its native telemetry generation. That
        // teardown must not turn the EOF we already handled into a new stop.
        if stopped
            && self.stopped_at.is_some()
            && self
                .owner
                .as_ref()
                .is_some_and(|(series, episode, session, _)| {
                    series == &target.series_id
                        && episode == &target.episode_id
                        && *session == target.session
                })
        {
            return None;
        }
        let owner = (
            target.series_id.clone(),
            target.episode_id.clone(),
            target.session,
            native,
        );
        if self.owner.as_ref() != Some(&owner) {
            *self = Self {
                owner: Some(owner),
                ..Self::default()
            };
        }
        if let Some(terminal) = self.stopped_at {
            // EOF can leave pause=true and position=duration for minutes.
            // Seeking back while paused or replaying flowing frames starts a
            // new viewing cycle on the same loaded native session.
            if !stopped && ((!paused && position < duration - 0.01) || position < terminal - 1.0) {
                self.replay = self.replay.wrapping_add(1);
                self.stopped_at = None;
            } else {
                return None;
            }
        }
        if stopped {
            self.stopped_at = Some(position);
        }
        Some(self.replay)
    }
}
#[derive(Debug)]
pub(super) enum ObservationError {
    Item(ApiError),
    Retry(ApiError),
}
impl From<ApiError> for ObservationError {
    fn from(error: ApiError) -> Self {
        Self::Retry(error)
    }
}
impl Bridge {
    /// Called with real player observations only; loading and position-only
    /// history imports never create scrobbles. The handle belongs to a profile.
    pub(in crate::app) fn list_services_playback(
        &self,
        target: &PlaybackTarget,
        position: f64,
        duration: f64,
        paused: bool,
        stopped: bool,
    ) {
        if !duration.is_finite() || duration <= 0.0 || !position.is_finite() || position < 0.0 {
            return;
        }
        let h = self.list_services();
        let native = self.player.screenshot_session();
        let replay = h
            .playback_end
            .lock()
            .unwrap()
            .observe(target, native, position, duration, paused, stopped);
        let Some(replay) = replay else {
            return;
        };
        let action = if stopped {
            ScrobbleAction::Stop
        } else if paused {
            ScrobbleAction::Pause
        } else {
            ScrobbleAction::Start
        };
        let observation = Observation {
            series_id: target.series_id.clone(),
            episode_id: target.episode_id.clone(),
            action,
            percent: (position / duration * 100.0).clamp(0.0, 100.0),
            at: now_secs(),
            session: target.session,
            native,
            replay,
        };
        let mut previous = h.playback.lock().unwrap();
        let unchanged = !stopped
            && previous.as_ref().is_some_and(|p| {
                p.series_id == observation.series_id
                    && p.episode_id == observation.episode_id
                    && p.session == observation.session
                    && p.native == observation.native
                    && p.replay == observation.replay
                    && p.action == observation.action
            });
        *previous = (!stopped).then(|| observation.clone());
        if unchanged {
            return;
        }
        drop(previous);
        let generation = h.generation[2].load(Ordering::Acquire);
        let mut pending = h.scrobbles.lock().unwrap();
        if pending
            .back()
            .is_some_and(|(p, g)| *g == generation && same_observation(p, &observation))
        {
            pending.pop_back();
        }
        // Bounded process-local retries retain terminal observations across
        // transient failures without feeding playback into the control lane.
        if pending.len() == 16 {
            let index = pending
                .iter()
                .position(|(o, _)| o.action != ScrobbleAction::Stop)
                .unwrap_or(0);
            pending.remove(index);
        }
        pending.push_back((observation, generation));
    }
}
impl Handle {
    pub(super) fn has_trakt_observation(&self, series: &str, episode: Option<&str>) -> bool {
        let matches =
            |o: &Observation| o.series_id == series && episode.is_none_or(|id| o.episode_id == id);
        self.playback.lock().unwrap().as_ref().is_some_and(&matches)
            || self
                .scrobbles
                .lock()
                .unwrap()
                .iter()
                .any(|(o, _)| matches(o))
    }
}
fn same_target(a: &Observation, b: &Observation) -> bool {
    a.series_id == b.series_id
        && a.episode_id == b.episode_id
        && a.session == b.session
        && a.native == b.native
        && a.replay == b.replay
}
fn same_observation(a: &Observation, b: &Observation) -> bool {
    a.series_id == b.series_id
        && a.episode_id == b.episode_id
        && a.session == b.session
        && a.native == b.native
        && a.replay == b.replay
        && a.action == b.action
}
impl Worker {
    pub(super) fn flush_trakt_observations(&mut self) {
        let h = self.bridge.list_services();
        if !self.automatic[2] {
            h.scrobbles.lock().unwrap().clear();
            return;
        }
        if !self.verified[2] || self.paused[2] || self.retry[2] > 5 && Instant::now() < self.next[2]
        {
            return;
        }
        let generation = h.generation[2].load(Ordering::Acquire);
        loop {
            let queued = h.scrobbles.lock().unwrap().pop_front();
            let Some((observation, owner)) = queued else {
                break;
            };
            if owner != generation
                || self
                    .rejected_scrobbles
                    .iter()
                    .any(|(failed, g)| *g == owner && same_target(failed, &observation))
                || self
                    .last_scrobble
                    .as_ref()
                    .is_some_and(|(last, g)| *g == owner && same_observation(last, &observation))
            {
                continue;
            }
            match self.trakt_observation(&observation, owner) {
                Ok(sent) => {
                    if sent {
                        self.last_scrobble = Some((observation, owner));
                    }
                }
                Err(ObservationError::Item(error)) => {
                    self.reject_trakt_observation(observation, owner, error)
                }
                Err(ObservationError::Retry(error)) => {
                    let mut pending = h.scrobbles.lock().unwrap();
                    if pending.len() == 16 {
                        pending.pop_back();
                    }
                    pending.push_front((observation, owner));
                    drop(pending);
                    self.failed(2, error);
                    return;
                }
            }
        }
        // Reconsider the newest real frame after verification, consent or
        // automatic-sync changes, even when no pause transition occurred.
        let live = h.playback.lock().unwrap().clone();
        if let Some(observation) = live
            && !self
                .rejected_scrobbles
                .iter()
                .any(|(failed, g)| *g == generation && same_target(failed, &observation))
            && !self
                .last_scrobble
                .as_ref()
                .is_some_and(|(last, g)| *g == generation && same_observation(last, &observation))
        {
            match self.trakt_observation(&observation, generation) {
                Ok(true) => self.last_scrobble = Some((observation, generation)),
                Ok(false) => {}
                Err(ObservationError::Item(error)) => {
                    self.reject_trakt_observation(observation, generation, error)
                }
                Err(ObservationError::Retry(error)) => self.failed(2, error),
            }
        }
    }
    fn reject_trakt_observation(
        &mut self,
        observation: Observation,
        generation: u64,
        _error: ApiError,
    ) {
        if self.rejected_scrobbles.len() == 16 {
            self.rejected_scrobbles.pop_front();
        }
        self.rejected_scrobbles.push_back((observation, generation));
        self.status[2] =
            text::tr("Unsupported or invalid service data. Some titles remain pending.").into();
        self.publish();
    }
    pub(super) fn begin_trakt_device(
        &mut self,
        client_id: String,
        generation: u64,
    ) -> Result<(), ApiError> {
        self.device = None;
        let background =
            tracking::sign_in_background_for("Trakt").map_err(|_| ApiError::Offline)?;
        let code = self.client.begin_trakt_device(&client_id)?;
        self.live(2, generation)?;
        self.status[2] = text::trakt_device_code(&code.verification_uri, &code.user_code);
        let uri = code.verification_uri.clone();
        publish(&self.bridge, move |_| {
            let _ = crate::player::open_browser(&uri);
        });
        self.busy[2] = true;
        self.paused[2] = false;
        self.device = Some(DeviceSession {
            client_id,
            expires: Instant::now() + Duration::from_secs(code.expires_in),
            next: Instant::now() + Duration::from_secs(code.interval),
            interval: code.interval,
            code,
            generation,
            _background: background,
        });
        Ok(())
    }
    pub(super) fn poll_trakt_device(&mut self) {
        let Some(mut session) = self.device.take() else {
            return;
        };
        if self.live(2, session.generation).is_err() {
            self.busy[2] = false;
            return;
        }
        if Instant::now() >= session.expires {
            self.busy[2] = false;
            self.status[2] = text::tr("Sign-in expired. Start again.").into();
            self.publish();
            return;
        }
        if Instant::now() < session.next {
            self.device = Some(session);
            return;
        }
        let result = self
            .client
            .poll_trakt_device(&session.client_id, &session.code.device_code);
        match result {
            Err(ApiError::Pending | ApiError::SlowDown) => {
                if matches!(result, Err(ApiError::SlowDown)) {
                    session.interval = (session.interval + 5).min(300);
                }
                session.next = Instant::now() + Duration::from_secs(session.interval);
                self.device = Some(session);
            }
            Ok(tokens) => {
                let result = (|| {
                    self.live(2, session.generation)?;
                    let credentials = Credentials {
                        service: Service::Trakt,
                        client_id: session.client_id,
                        token: tokens.access_token,
                    };
                    let account = self.client.verify(&credentials)?;
                    self.live(2, session.generation)?;
                    let connection = Connection {
                        version: 1,
                        credentials,
                        account,
                        epoch: format!(
                            "{}:{}",
                            nova_config::now_ms(),
                            nova_sync::local_device().map_err(|_| ApiError::Rejected)?
                        ),
                        refresh: Some(Refresh {
                            token: tokens.refresh_token,
                            expires_at: now_secs().saturating_add(tokens.expires_in),
                        }),
                    };
                    let preferences = self.install_connection(2, &connection)?;
                    self.connections[2] = Some(connection);
                    self.verified[2] = true;
                    self.full[2] = preferences.full;
                    self.automatic[2] = preferences.automatic;
                    self.next[2] = Instant::now();
                    self.retry[2] = 5;
                    self.status[2] = text::tr("Connected").into();
                    Ok(())
                })();
                self.busy[2] = false;
                if let Err(error) = result {
                    self.failed(2, error);
                }
                self.publish();
            }
            Err(error) => {
                self.busy[2] = false;
                self.status[2] = text::tr(if error == ApiError::Expired {
                    "Sign-in expired. Start again."
                } else {
                    "Service unavailable. Try again."
                })
                .into();
                self.publish();
            }
        }
    }
    pub(super) fn refresh_trakt(&mut self) -> Result<(), ApiError> {
        if self.paused[2]
            || self.device.is_some()
            || self.retry[2] > 5 && Instant::now() < self.next[2]
        {
            return Ok(());
        }
        let Some(old) = self.connections[2].clone() else {
            return Ok(());
        };
        if old.credentials.service != Service::Trakt {
            return Err(ApiError::InvalidResponse);
        }
        let Some(refresh) = &old.refresh else {
            return Ok(());
        };
        if now_secs().saturating_add(60) < refresh.expires_at {
            return Ok(());
        }
        let generation = self.bridge.list_services().generation[2].load(Ordering::Acquire);
        self.current(2, generation, &old)?;
        let tokens = match self
            .client
            .refresh_trakt(&old.credentials.client_id, &refresh.token)
        {
            Ok(tokens) => tokens,
            Err(ApiError::Authentication) => {
                // A paired device may have consumed this single-use token.
                // Keep the connection, ask for peer updates and retry later;
                // never tombstone it or overwrite a newer shared credential.
                self.verified[2] = false;
                nova_sync::request_local_sync();
                return Err(ApiError::Pending);
            }
            Err(error) => return Err(error),
        };
        self.live(2, generation)?;
        let mut updated = old.clone();
        updated.credentials.token = tokens.access_token;
        updated.refresh = Some(Refresh {
            token: tokens.refresh_token,
            expires_at: now_secs().saturating_add(tokens.expires_in),
        });
        // A refresh token is single-use: save its replacement before any other
        // request. Never resurrect a disconnect or overwrite newer peer auth.
        let owner = nova_sync::local_store().map_err(|_| ApiError::Rejected)?;
        let device = nova_sync::local_device().map_err(|_| ApiError::Rejected)?;
        let mut store = owner.lock().unwrap();
        let current = stored::<Connection>(&store, "connection:2")?;
        if current
            .as_ref()
            .is_none_or(|c| !same_session(c, &old) || c.refresh != old.refresh)
        {
            return Err(ApiError::Authentication);
        }
        store.set(
            &sync::profile_domain(DOMAIN),
            "connection:2",
            Some(serde_json::to_string(&updated).map_err(|_| ApiError::InvalidInput)?),
            0,
            device,
        );
        store.save().map_err(|_| ApiError::Rejected)?;
        drop(store);
        nova_sync::request_local_sync();
        self.connections[2] = Some(updated);
        self.verified[2] = false;
        Ok(())
    }
    pub(super) fn trakt_observation(
        &mut self,
        observation: &Observation,
        generation: u64,
    ) -> Result<bool, ObservationError> {
        if !self.verified[2]
            || !self.automatic[2]
            || self.paused[2]
            || self.retry[2] > 5 && Instant::now() < self.next[2]
        {
            return Ok(false);
        }
        let connection = self.connections[2]
            .clone()
            .ok_or(ApiError::Authentication)?;
        self.current(2, generation, &connection)?;
        let Some(entry) = self.bridge.personal_state().and_then(|s| {
            s.entries
                .iter()
                .find(|e| e.id == observation.series_id)
                .cloned()
        }) else {
            return Ok(false);
        };
        let Some(source) = source(entry) else {
            return Ok(false);
        };
        let Some(selection) =
            self.enabled(2, &connection, &source.entry.type_, &source.entry.id)?
        else {
            return Ok(false);
        };
        // An observation captured before a newly granted selection cannot gain
        // upload permission by waiting in the actor's queue.
        if !selection.grant.is_empty() && observation.at <= selection.since {
            return Ok(false);
        }
        let episode = if source.kind == MediaType::Movie {
            None
        } else {
            let Some((_, episode)) = source
                .episodes
                .iter()
                .find(|(id, _)| id == &observation.episode_id)
            else {
                return Ok(false);
            };
            Some(*episode)
        };
        if observation.action == ScrobbleAction::Stop && observation.percent < 1.0 {
            return Ok(false);
        }
        self.scope_current(2, generation, &connection, &source, &selection)?;
        self.client
            .scrobble(
                &connection.credentials,
                &source.ids,
                source.kind,
                episode,
                observation.percent,
                observation.action,
            )
            .map_err(|error| {
                // Local cancellation and changing scope are retryable; an invalid
                // catalog item must not hold unrelated titles behind it forever.
                if matches!(
                    error,
                    ApiError::Unsupported
                        | ApiError::InvalidInput
                        | ApiError::InvalidResponse
                        | ApiError::Rejected
                ) && self.gate(2, generation).is_ok()
                {
                    ObservationError::Item(error)
                } else {
                    ObservationError::Retry(error)
                }
            })?;
        Ok(true)
    }
}

#[cfg(test)]
impl Refresh {
    pub(super) fn test_expired() -> Self {
        Self {
            token: Secret::new("old-refresh".into()).unwrap(),
            expires_at: 1,
        }
    }
    pub(super) fn test_fresh() -> Self {
        Self {
            token: Secret::new("peer-refresh".into()).unwrap(),
            expires_at: now_secs() + 604800,
        }
    }
}
#[cfg(test)]
impl Observation {
    pub(super) fn test_movie(id: &str, action: ScrobbleAction) -> Self {
        Self {
            series_id: id.into(),
            episode_id: id.into(),
            action,
            percent: 50.0,
            session: 1,
            native: 0,
            replay: 0,
            at: now_secs(),
        }
    }
}

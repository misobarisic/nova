//! A temporary party shares a timeline, never a stream or a sync relationship.
//! The UI thread owns player access. One worker operation at a time owns all
//! room I/O, while local drift corrections use the latest immutable snapshot.
use super::*;
use nova_party::{
    ControlPolicy, HostCommand, LocalReport, PartyMedia, PartyRole, PartyService, RoomSnapshot,
};
#[cfg(test)]
use nova_playback_api::MediaIdentity;
use nova_playback_api::PlaybackAction;
use std::time::Instant;
use unicode_normalization::UnicodeNormalization;

fn party_episode_caption(state: &Shared, media: &PartyMedia) -> String {
    if spoiler::hidden(
        state,
        &media.identity.media_type,
        &media.identity.title_id,
        &media.identity.episode_id,
    ) {
        match (media.season, media.episode) {
            (Some(season), Some(episode)) => format!("S{season} E{episode}"),
            (_, Some(episode)) => format!("E{episode}"),
            _ => text::tr("Episode").into(),
        }
    } else {
        media.identity.episode.clone()
    }
}

fn participant_initial(name: &str) -> SharedString {
    name.trim()
        .nfc()
        .next()
        .and_then(|c| c.to_uppercase().next())
        .unwrap_or('?')
        .to_string()
        .into()
}

#[derive(Default)]
pub(super) struct State {
    service: Option<Arc<PartyService>>,
    snapshot: Option<RoomSnapshot>,
    generation: u64,
    busy: bool,
    pending: std::collections::VecDeque<Operation>,
    last_report: Option<Instant>,
    last_operation_was_report: bool,
    last_guest_control_finished: Option<Instant>,
    control_wakeup: Option<ControlWakeup>,
    /// A user explicitly chose to find this room's episode in their own addons.
    choosing_source: bool,
    /// The creator must validate a source before activating pending media.
    choosing_media: bool,
    selection_epoch: u64,
    selection_revision: u64,
    followed_generation: Option<u64>,
    last_seek: Option<(Instant, f64)>,
    resume_guard: bool,
    setup_restore: Option<(u64, bool)>,
    source_choice: Option<SourceChoice>,
    selection: Option<Selection>,
}

#[derive(Clone)]
struct SourceChoice {
    activation: u64,
    generation: u64,
    media: nova_playback_api::MediaIdentity,
    fingerprint: nova_party::SourceFingerprint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ControlWakeup {
    activation: u64,
    room_id: String,
    generation: u64,
}

impl ControlWakeup {
    fn current(&self, state: &State) -> bool {
        state.active()
            && state.generation == self.activation
            && state.snapshot_for_sources().as_ref().is_some_and(|room| {
                room.room_id == self.room_id && room.generation == self.generation
            })
    }
}

impl State {
    fn push_operation(&mut self, operation: Operation) -> bool {
        // Absolute slider moves supersede only an adjacent absolute move in
        // the same room state. Steps, resets and other actions keep ordering.
        if let Operation::Command(generation, revision, HostCommand::SetRate(_)) = &operation
            && let Some(Operation::Command(old_generation, old_revision, HostCommand::SetRate(_))) =
                self.pending.back()
            && generation == old_generation
            && revision == old_revision
        {
            *self.pending.back_mut().unwrap() = operation;
            return true;
        }
        if self.pending.len() >= 32 {
            return false;
        }
        self.pending.push_back(operation);
        true
    }

    fn report_should_run(&self, now: Instant) -> bool {
        self.last_report
            .is_none_or(|at| now.saturating_duration_since(at) >= Duration::from_millis(500))
            && (!self.last_operation_was_report || self.pending.is_empty())
    }

    fn note_operation_dispatched(&mut self, operation: &Operation, now: Instant) {
        self.last_operation_was_report = matches!(operation, Operation::Report(_));
        if self.last_operation_was_report {
            self.last_report = Some(now);
        }
    }

    fn guest_control_wait(&self, now: Instant) -> Option<Duration> {
        if !self.pending.front().is_some_and(Operation::is_control)
            || !self
                .snapshot_for_sources()
                .is_some_and(|room| room.role == PartyRole::Guest)
        {
            return None;
        }
        Duration::from_millis(50)
            .checked_sub(now.saturating_duration_since(self.last_guest_control_finished?))
            .filter(|wait| !wait.is_zero())
    }

    pub(super) fn snapshot_for_sources(&self) -> Option<RoomSnapshot> {
        // A network reply can revoke permissions before its UI continuation
        // runs. Authorize from the service; retain the last read on contention.
        self.service
            .as_ref()
            .and_then(|service| service.snapshot())
            .or_else(|| self.snapshot.clone())
    }

    fn retire_old_operations(&mut self, generation: u64) {
        self.pending.retain(|operation| match operation {
            Operation::Command(expected, ..)
            | Operation::PrepareMedia(expected, ..)
            | Operation::SelectMedia(expected, ..) => *expected == generation,
            Operation::Report(report) => report.generation == generation,
            _ => true,
        });
    }

    fn prepared_selection_matches(&self, room: &RoomSnapshot, expected: u64) -> bool {
        expected.checked_add(1) == Some(room.generation)
            && self.selection.as_ref().is_some_and(|selection| {
                selection.guard.generation == expected
                    && selection.guard.room_id == room.room_id
                    && room.selected_media().is_some_and(|media| {
                        party_source::same_media(&media.identity, &selection.media.identity)
                    })
            })
    }

    fn reject_prepared_selection(&mut self) {
        if self
            .selection
            .as_ref()
            .is_some_and(|selection| !selection.accepted)
        {
            self.selection = None;
        }
    }
    fn accept_prepared_selection(
        &mut self,
        room: &RoomSnapshot,
        expected: u64,
    ) -> (Option<String>, Option<(NextSelection, PlaybackGuard)>) {
        if !self.prepared_selection_matches(room, expected) {
            return (None, None);
        }
        let Some(selection) = self.selection.as_mut() else {
            return (None, None);
        };
        selection.guard.generation = room.generation;
        selection.accepted = true;
        (
            selection.download.take(),
            selection
                .next
                .take()
                .map(|next| (next, selection.guard.clone())),
        )
    }

    pub(super) fn active(&self) -> bool {
        self.service.is_some() || self.busy
    }
}

/// Room identity and activation distinguish leaving/rejoining and A → B → A.
#[derive(Clone)]
pub(super) struct PlaybackGuard {
    activation: u64,
    room_id: String,
    generation: u64,
    media: nova_playback_api::MediaIdentity,
}

#[derive(Clone)]
struct Selection {
    guard: PlaybackGuard,
    media: PartyMedia,
    owner: Option<Arc<()>>,
    request_id: String,
    accepted: bool,
    download: Option<String>,
    next: Option<NextSelection>,
}

#[derive(Clone)]
struct NextSelection {
    owner: Arc<()>,
    local_session: u64,
    native_session: u64,
    series: String,
    current: String,
    episode: String,
}

fn can_control(room: &RoomSnapshot) -> bool {
    room.connected
        && !room.ended
        && (room.role == PartyRole::Host || room.control_policy == ControlPolicy::Everyone)
}

impl PlaybackGuard {
    pub(super) fn accepts(&self, state: &State) -> bool {
        state.active()
            && state.generation == self.activation
            && state.snapshot_for_sources().as_ref().is_some_and(|room| {
                room.room_id == self.room_id
                    && room.generation == self.generation
                    && room.connected
                    && !room.ended
                    && room
                        .selected_media()
                        .is_some_and(|media| party_source::same_media(&media.identity, &self.media))
            })
    }
}

enum Operation {
    Command(u64, u64, HostCommand),
    Policy(ControlPolicy),
    ClearMedia { browse: bool },
    PrepareMedia(u64, u64, PartyMedia),
    SelectMedia(u64, PartyMedia, LocalReport),
    Kick(String),
    Wait(bool),
    Continue,
    Report(LocalReport),
}

impl Operation {
    fn is_control(&self) -> bool {
        matches!(self, Self::Command(..) | Self::PrepareMedia(..))
    }
}

fn normalized_title(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn public_title_id(value: &str) -> bool {
    value
        .strip_prefix("tt")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        || ["tmdb:", "kitsu:", "anilist:"]
            .iter()
            .any(|prefix| value.starts_with(prefix))
}

/// Exact confirmed identities win. An explicit manual source choice may use
/// equal title/episode numbering for private addon IDs, with an edition warning;
/// conflicting public identities must never be overruled by a display title.
fn compatible_media(local: &PartyMedia, expected: &PartyMedia, manual: bool) -> bool {
    let movie = local.identity.media_type == "movie";
    if movie != (expected.identity.media_type == "movie") {
        return false;
    }
    if local.identity.title_id == expected.identity.title_id
        && local.identity.episode_id == expected.identity.episode_id
    {
        return true;
    }
    if public_title_id(&local.identity.title_id) && public_title_id(&expected.identity.title_id) {
        return false;
    }
    manual
        && !normalized_title(&local.identity.title).is_empty()
        && normalized_title(&local.identity.title) == normalized_title(&expected.identity.title)
        && (movie
            || (local.season.is_some()
                && local.episode.is_some()
                && local.season == expected.season
                && local.episode == expected.episode))
}

fn bind_host_activation(media: &mut PartyMedia, report: &mut LocalReport, expected: &PartyMedia) {
    media.identity = expected.identity.clone();
    report.media = Some(expected.identity.clone());
}

impl Bridge {
    pub(super) fn party_active(&self) -> bool {
        let state = self.shared.lock().unwrap();
        state.party.service.is_some() || state.party.busy
    }

    /// Local history never owns a party source's initial position. Resolve the
    /// actual playback intent after releasing Shared: party_media locks it too.
    pub(super) fn party_start_position(&self) -> Option<f64> {
        let (room, manual) = {
            let state = self.shared.lock().unwrap();
            let party = &state.party;
            if !party.active() {
                return None;
            }
            (party.snapshot.clone()?, party.choosing_source)
        };
        if room.pending_media.is_some() && self.shared.lock().unwrap().party.choosing_media {
            return Some(0.0);
        }
        let media = room.selected_media()?;
        let local = self.party_media()?;
        compatible_media(&local, media, manual).then(|| {
            room.timeline
                .position_at(nova_party::local_now_ms() as f64 + room.clock_offset_ms)
                .clamp(0.0, media.duration)
        })
    }

    pub(super) fn party_remember_source(&self, stream: &StreamUi) {
        let fingerprint = party_source::fingerprint(&stream.source);
        let mut state = self.shared.lock().unwrap();
        let media = state
            .modal_item
            .as_ref()
            .map(|m| nova_playback_api::MediaIdentity {
                media_type: m.type_.clone(),
                title_id: m.id.clone(),
                episode_id: m.request_id.clone(),
                ..Default::default()
            });
        let party = &mut state.party;
        party.source_choice = if party.active() {
            media
                .zip(fingerprint)
                .map(|(media, fingerprint)| SourceChoice {
                    activation: party.generation,
                    generation: party.snapshot.as_ref().map_or(0, |r| r.generation),
                    media,
                    fingerprint,
                })
        } else {
            None
        };
    }

    fn party_chosen_source(
        &self,
        room: Option<&RoomSnapshot>,
        local: Option<&PartyMedia>,
    ) -> Option<nova_party::SourceFingerprint> {
        let current = self.current_playback_identity();
        let mut state = self.shared.lock().unwrap();
        let party = &mut state.party;
        let choice = party.source_choice.as_ref()?;
        if choice.activation != party.generation
            || !current
                .as_ref()
                .is_some_and(|media| party_source::same_media(media, &choice.media))
        {
            party.source_choice = None;
            return None;
        }
        if let Some(room) = room {
            if !room.connected
                || room.ended
                || choice.generation != room.generation
                || !room
                    .selected_media()
                    .zip(local)
                    .is_some_and(|(expected, local)| {
                        party_source::same_media(&expected.identity, &local.identity)
                    })
            {
                party.source_choice = None;
                return None;
            }
        } else if !party.choosing_media {
            return None;
        }
        Some(choice.fingerprint)
    }

    pub(super) fn refresh_party_stream_recommendations(&self) {
        let Some(app) = self.app() else { return };
        let owner = profiles::ProfileStore::capture();
        let (room, media, rows, title_allowed) = {
            let state = self.shared.lock().unwrap();
            let media = state.modal_item.as_ref().map(|m| {
                party_source::canonical_media(
                    nova_playback_api::MediaIdentity {
                        media_type: m.type_.clone(),
                        title_id: m.id.clone(),
                        episode_id: m.request_id.clone(),
                        ..Default::default()
                    },
                    &m.videos,
                )
            });
            let title_allowed = state
                .modal_item
                .as_ref()
                .is_some_and(|m| profile_policy::allows_title(&owner, &m.type_, &m.id));
            (
                state.party.snapshot.clone(),
                media,
                state.streams.clone(),
                title_allowed,
            )
        };
        let model = app.get_streams();
        for (index, source) in rows.iter().enumerate() {
            let Some(mut row) = model.row_data(index).filter(|row| row.id == source.id) else {
                continue;
            };
            let source_allowed = source.download.as_ref().map_or_else(
                || profile_policy::allows_addon(&owner, &source.owner_url),
                |job| smart_offline::allows_download(&owner, job),
            );
            let value = title_allowed
                && source_allowed
                && media.as_ref().is_some_and(|media| {
                    party_source::recommended(room.as_ref(), media, &source.source)
                });
            if row.party_recommended != value {
                row.party_recommended = value;
                model.set_row_data(index, row);
            }
        }
    }

    pub(super) fn wire_party(&self) {
        let Some(app) = self.app() else { return };
        let b = self.clone();
        app.on_party_create(move || b.party_begin(None));
        let b = self.clone();
        app.on_party_join(move |invite| b.party_begin(Some(invite.to_string())));
        let b = self.clone();
        app.on_party_leave(move || b.party_leave());
        let b = self.clone();
        app.on_party_choose_stream(move || b.party_choose_stream());
        let b = self.clone();
        app.on_party_reveal_spoilers(move || {
            let media = b
                .shared
                .lock()
                .unwrap()
                .party
                .snapshot
                .as_ref()
                .and_then(|room| room.selected_media().cloned());
            if let Some(media) = media {
                spoiler::reveal(
                    &media.identity.media_type,
                    &media.identity.title_id,
                    &media.identity.episode_id,
                );
                b.refresh_spoiler_presentation();
            }
        });
        let b = self.clone();
        app.on_party_choose_media(move || b.party_prepare_media_change(true));
        let b = self.clone();
        app.on_party_copy_invite(move || {
            if let Some(app) = b.app() {
                copy_to_clipboard(app.get_party_invite().as_str());
            }
        });
        let b = self.clone();
        app.on_party_return_to_player(move || {
            if let Some(app) = b.app() {
                app.set_party_page_open(false);
            }
        });
        let b = self.clone();
        app.on_party_set_allow_everyone(move |everyone| {
            if b.party_active() {
                b.party_enqueue(Operation::Policy(if everyone {
                    ControlPolicy::Everyone
                } else {
                    ControlPolicy::HostOnly
                }));
            } else if let Some(app) = b.app() {
                // Before Create this is only a draft for the new room. Joining
                // always takes its policy from the creator's snapshot.
                app.set_party_allow_everyone(everyone);
            }
        });
        let b = self.clone();
        app.on_party_set_target_speed(move |rate| {
            if rate.is_finite() {
                b.party_command(HostCommand::SetRate(f64::from(rate)));
            }
        });
        let b = self.clone();
        app.on_party_step_target_speed(move |direction| {
            b.party_command(HostCommand::StepRate(direction))
        });
        let b = self.clone();
        app.on_party_reset_target_speed(move || b.party_command(HostCommand::ResetRate));
        let b = self.clone();
        app.on_party_set_paused(move |paused| b.party_command(HostCommand::SetPaused(paused)));
        let b = self.clone();
        app.on_party_seek(move |position| {
            if position.is_finite() {
                b.party_command(HostCommand::SeekAbsolute(position as f64));
            }
        });
        let b = self.clone();
        app.on_party_remove_member(move |id| b.party_enqueue(Operation::Kick(id.to_string())));
        let b = self.clone();
        app.on_party_set_wait_for_guests(move |enabled| b.party_enqueue(Operation::Wait(enabled)));
        let b = self.clone();
        app.on_party_continue_without_waiting(move || b.party_enqueue(Operation::Continue));
    }

    fn party_media(&self) -> Option<PartyMedia> {
        let app = self.app()?;
        let mut identity = self.current_playback_identity()?;
        let videos = {
            let state = self.shared.lock().unwrap();
            state
                .modal_item
                .as_ref()
                .filter(|m| m.id == identity.title_id)
                .map(|m| m.videos.clone())
        }
        .or_else(|| read_episodes_cache_for(&identity.media_type, &identity.title_id))
        .unwrap_or_default();
        let video = videos.iter().find(|v| v.id == identity.episode_id);
        let (season, episode) = video
            .map(|v| (v.season, v.episode.or(v.number)))
            .unwrap_or_default();
        identity = party_source::canonical_media(identity, &videos);
        Some(PartyMedia {
            identity,
            season,
            episode,
            duration: app.get_duration().max(0.0) as f64,
        })
    }

    fn party_report(&self, room: Option<&RoomSnapshot>) -> LocalReport {
        let Some(app) = self.app() else {
            return LocalReport::default();
        };
        if let Some(room) = room
            && room.selected_media().is_none()
        {
            return LocalReport {
                generation: room.generation,
                ..Default::default()
            };
        }
        let local = self.party_media();
        let manual = self.shared.lock().unwrap().party.choosing_source;
        let matches = room.is_none_or(|room| {
            local.as_ref().is_some_and(|local| {
                room.selected_media()
                    .is_some_and(|media| compatible_media(local, media, manual))
            })
        });
        let readiness = self.player.playback_readiness();
        let loaded =
            app.get_player_open() && app.get_playback_started() && !app.get_resume_prompt_visible();
        let media = matches
            .then(|| {
                room.and_then(|r| r.selected_media().map(|media| media.identity.clone()))
                    .or_else(|| local.as_ref().map(|m| m.identity.clone()))
            })
            .flatten();
        LocalReport {
            source_fingerprint: self.party_chosen_source(room, local.as_ref()),
            generation: room.map_or(0, |room| room.generation),
            media,
            loaded,
            seekable: readiness.seekable,
            position: app.get_position().max(0.0) as f64,
            duration: app.get_duration().max(0.0) as f64,
            buffered_ahead: readiness.buffered_ahead,
            buffering: readiness.buffering,
            ended: readiness.ended,
            ready: loaded && matches && readiness.seekable && !readiness.buffering,
            error: (!matches).then(|| "choose a local stream".into()),
        }
    }

    fn party_creation_policy(&self, joining: bool) -> ControlPolicy {
        if !joining && self.app().is_some_and(|app| app.get_party_allow_everyone()) {
            ControlPolicy::Everyone
        } else {
            ControlPolicy::HostOnly
        }
    }

    fn party_begin(&self, invite: Option<String>) {
        if self.shared.lock().unwrap().casting.reserved() {
            if let Some(app) = self.app() {
                app.set_party_error(
                    text::tr("Stop casting before joining a watch party or paired Nova session.")
                        .into(),
                );
            }
            return;
        }
        if self.party_active() {
            return;
        }
        let Some(app) = self.app() else { return };
        if self.remote_output_selected() || app.get_remote_controls_open() {
            app.set_party_error(
                text::tr("Disconnect remote controls before joining a watch party.").into(),
            );
            return;
        }
        if invite
            .as_ref()
            .is_some_and(|ticket| ticket.len() > 8192 || ticket.trim().is_empty())
        {
            app.set_party_error(text::tr("The watch-party invite is invalid or expired.").into());
            return;
        }
        let was_paused = app.get_is_paused();
        let creation_policy = self.party_creation_policy(invite.is_some());
        let playback_epoch = self.player.screenshot_session();
        let generation = {
            let mut state = self.shared.lock().unwrap();
            let p = &mut state.party;
            p.generation = p.generation.wrapping_add(1);
            p.busy = true;
            p.choosing_source = false;
            p.setup_restore = app
                .get_player_open()
                .then_some((playback_epoch, was_paused));
            p.generation
        };
        app.set_party_busy(true);
        app.set_party_error("".into());
        app.set_party_invite_qr(Image::default());
        self.release_speed_preview_for_party();
        // Pause unrelated playback while creating/joining the lobby; only a
        // subsequent source selection can bind it to the room.
        if app.get_player_open() {
            self.player.set_paused(true);
        }
        let name = nova_sync::read_settings().device_name;
        let mut name = name.chars().filter(|c| !c.is_control()).collect::<String>();
        while name.len() > 64 {
            name.pop();
        }
        let name = if name.trim().is_empty() {
            "Nova".to_string()
        } else {
            name
        };
        let invite_is_host = invite.is_none();
        let b = self.clone();
        thread::spawn(move || {
            let result = PartyService::new().and_then(|service| {
                let ticket = if let Some(invite) = invite {
                    service.join(invite.trim(), name)?;
                    String::new()
                } else {
                    let ticket = service.create_lobby(name)?;
                    // Publish neither the invite nor the first snapshot until
                    // the creator's selected initial permissions are applied.
                    service.set_control_policy(creation_policy)?;
                    ticket
                };
                Ok((service, ticket))
            });
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                if b.shared.lock().unwrap().party.generation != generation {
                    if let Ok((service, _)) = result {
                        thread::spawn(move || {
                            let _ = service.leave();
                        });
                    }
                    return;
                }
                let Some(app) = b.app() else { return };
                app.set_party_busy(false);
                match result {
                    Ok((service, ticket)) => {
                        let snapshot = service.snapshot();
                        {
                            let mut state = b.shared.lock().unwrap();
                            state.party.busy = false;
                            state.party.setup_restore = None;
                            state.party.service = Some(service);
                            state.party.snapshot = snapshot;
                            state.party.choosing_media = invite_is_host;
                            state.party.selection_epoch = playback_epoch;
                        }
                        b.release_speed_preview_for_party();
                        b.player.set_party_playback_rate(Some(1.0));
                        app.set_party_invite_qr(if invite_is_host {
                            invite_qr_image(&ticket).unwrap_or_default()
                        } else {
                            Image::default()
                        });
                        app.set_party_invite(ticket.into());
                        app.set_party_active(true);
                        b.party_publish();
                        b.party_tick();
                    }
                    Err(error) => {
                        let restore = {
                            let mut state = b.shared.lock().unwrap();
                            state.party.busy = false;
                            state.party.setup_restore.take()
                        };
                        b.party_restore_setup(restore);
                        app.set_party_error(
                            text::tr(if error.to_string().contains("invite") {
                                "The watch-party invite is invalid or expired."
                            } else {
                                "Could not connect to the watch party. Try again."
                            })
                            .into(),
                        );
                    }
                }
            });
        });
    }

    fn party_restore_setup(&self, restore: Option<(u64, bool)>) {
        if let Some((epoch, paused)) = restore
            && self.player.screenshot_session() == epoch
            && self.app().is_some_and(|app| app.get_player_open())
        {
            self.player.set_paused(paused);
        }
    }

    pub(super) fn party_leave(&self) {
        let (service, restore) = {
            let mut state = self.shared.lock().unwrap();
            let generation = state.party.generation.wrapping_add(1);
            let service = state.party.service.take();
            let restore = state.party.setup_restore.take();
            state.party = State {
                generation,
                ..Default::default()
            };
            (service, restore)
        };
        self.player.set_party_playback_rate(None);
        self.party_restore_setup(restore);
        self.refresh_party_stream_recommendations();
        if let Some(app) = self.app() {
            app.set_party_active(false);
            app.set_party_can_control(false);
            app.set_party_can_choose_media(false);
            app.set_party_allow_everyone(false);
            app.set_party_target_speed(1.0);
            app.set_party_has_media(false);
            app.set_party_can_return_to_player(false);
            app.set_party_busy(false);
            app.set_party_waiting(false);
            app.set_party_invite("".into());
            app.set_party_invite_qr(Image::default());
            app.set_party_participants(Default::default());
            app.set_party_status("".into());
            app.set_party_error("".into());
            app.set_party_warning("".into());
            if app.get_party_page_open() {
                app.recover_back_focus();
            }
        }
        // Runtime shutdown and the guest's best-effort leave never block Slint.
        if let Some(service) = service {
            thread::spawn(move || {
                let _ = service.leave();
            });
        }
    }

    fn party_selection_error(&self, stale: bool) {
        if let Some(app) = self.app() {
            app.set_party_error(text::tr(if stale {
                "This watch-party selection changed. Choose a stream again."
            } else {
                "The host chooses what to watch. Choose a stream for the current episode or leave the party."
            }).into());
        }
    }

    /// Run before changing playback targets, picker state or querying streams.
    /// Browsing and download-only selectors never announce a room selection.
    pub(super) fn party_preflight_media(&self, media: PartyMedia) -> bool {
        let kind = media.identity.media_type.clone();
        let title = media.identity.title_id.clone();
        self.party_preflight_media_for_title(media, &kind, &title)
    }

    /// Authorization belongs to the original provider title. Its confirmed
    /// canonical room identity need not be a separately allowlisted title.
    pub(super) fn party_preflight_media_for_title(
        &self,
        media: PartyMedia,
        kind: &str,
        title: &str,
    ) -> bool {
        self.party_preflight_media_inner(media, kind, title, None)
    }

    fn party_preflight_media_inner(
        &self,
        media: PartyMedia,
        kind: &str,
        title: &str,
        expected: Option<&PlaybackGuard>,
    ) -> bool {
        if self.party_active()
            && !profile_policy::allows_browse_title(&profiles::ProfileStore::capture(), kind, title)
        {
            return false;
        }
        let mut state = self.shared.lock().unwrap();
        if state.season_download.is_some() || !state.party.active() {
            return true;
        }
        let party = &mut state.party;
        let Some(room) = party
            .snapshot_for_sources()
            .filter(|room| room.connected && !room.ended)
        else {
            drop(state);
            self.party_selection_error(true);
            return false;
        };
        if expected.is_some_and(|expected| {
            expected.activation != party.generation
                || expected.room_id != room.room_id
                || expected.generation != room.generation
        }) {
            drop(state);
            self.party_selection_error(true);
            return false;
        }
        let matching = room
            .selected_media()
            .is_some_and(|expected| compatible_media(&media, expected, party.choosing_source));
        if !matching && !can_control(&room) {
            drop(state);
            self.party_selection_error(false);
            return false;
        }
        // Repeated entry must not submit a second proposal against the
        // generation its first proposal is in the process of replacing.
        if party.selection.as_ref().is_some_and(|selection| {
            !selection.accepted
                && party_source::same_media(&selection.media.identity, &media.identity)
                && selection.guard.activation == party.generation
                && selection.guard.room_id == room.room_id
                && selection.guard.generation == room.generation
        }) {
            return true;
        }
        let expected = if matching {
            room.selected_media().unwrap()
        } else {
            &media
        };
        let cancelled_next = party
            .selection
            .as_ref()
            .is_some_and(|selection| selection.next.is_some());
        party.selection_revision = party.selection_revision.wrapping_add(1);
        party.selection = Some(Selection {
            guard: PlaybackGuard {
                activation: party.generation,
                room_id: room.room_id.clone(),
                generation: room.generation,
                media: expected.identity.clone(),
            },
            media: media.clone(),
            owner: None,
            request_id: media.identity.episode_id.clone(),
            accepted: matching,
            download: None,
            next: None,
        });
        party.followed_generation = Some(room.generation);
        if matching && room.role == PartyRole::Host && room.pending_media.is_some() {
            party.choosing_media = true;
            party.selection_epoch = self.player.screenshot_session();
        }
        if !matching {
            party.choosing_media = room.role == PartyRole::Host;
            party.choosing_source = false;
            party.source_choice = None;
            party.selection_epoch = self.player.screenshot_session();
            party.pending.retain(|op| {
                matches!(
                    op,
                    Operation::Kick(_) | Operation::Wait(_) | Operation::Policy(_)
                )
            });
            party.pending.push_front(Operation::PrepareMedia(
                room.generation,
                room.control_revision,
                media,
            ));
        }
        drop(state);
        if cancelled_next && let Some(app) = self.app() {
            app.set_party_busy(false);
        }
        self.party_tick();
        true
    }

    pub(super) fn party_modal_media(&self, request_id: &str) -> Option<PartyMedia> {
        let state = self.shared.lock().unwrap();
        let modal = state.modal_item.as_ref()?;
        let video = modal.videos.iter().find(|video| video.id == request_id);
        Some(PartyMedia {
            identity: party_source::canonical_media(
                nova_playback_api::MediaIdentity {
                    media_type: modal.type_.clone(),
                    title_id: modal.id.clone(),
                    episode_id: request_id.into(),
                    title: modal.name.clone(),
                    episode: video.map(|v| v.name.clone()).unwrap_or_default(),
                },
                &modal.videos,
            ),
            season: video.and_then(|v| v.season),
            episode: video.and_then(|v| v.episode.or(v.number)),
            duration: 0.0,
        })
    }

    pub(super) fn party_stream_lookup_allowed(&self, request_id: &str) -> bool {
        let media = self.party_modal_media(request_id);
        let state = self.shared.lock().unwrap();
        if !state.party.active() || state.season_download.is_some() {
            return true;
        }
        state
            .party
            .snapshot_for_sources()
            .as_ref()
            .is_some_and(|room| {
                can_control(room)
                    || (room.connected
                        && !room.ended
                        && media.as_ref().zip(room.selected_media()).is_some_and(
                            |(local, expected)| {
                                compatible_media(local, expected, state.party.choosing_source)
                            },
                        ))
            })
    }

    pub(super) fn party_preflight_modal(&self, request_id: &str) -> bool {
        if !self.party_active() {
            return true;
        }
        let authority = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .map(|modal| (modal.type_.clone(), modal.id.clone()));
        authority
            .zip(self.party_modal_media(request_id))
            .is_some_and(|((kind, title), media)| {
                self.party_preflight_media_for_title(media, &kind, &title)
            })
    }

    /// Bind after navigation installs the modal. Equal IDs in a new opening
    /// must not let an older resolver or proposal own that new opening.
    pub(super) fn party_bind_picker(&self) {
        let mut state = self.shared.lock().unwrap();
        let Some(modal) = state.modal_item.as_ref() else {
            return;
        };
        let token = modal.open_token.clone();
        let request_id = modal.request_id.clone();
        let identity = party_source::canonical_media(
            nova_playback_api::MediaIdentity {
                media_type: modal.type_.clone(),
                title_id: modal.id.clone(),
                episode_id: request_id.clone(),
                ..Default::default()
            },
            &modal.videos,
        );
        if let Some(selection) = state.party.selection.as_mut()
            && party_source::same_media(&selection.media.identity, &identity)
        {
            selection.owner = Some(token);
            selection.request_id = request_id;
        }
    }

    pub(super) fn party_playback_guard(&self) -> Result<Option<PlaybackGuard>, ()> {
        let state = self.shared.lock().unwrap();
        if !state.party.active() {
            return Ok(None);
        }
        let selection = state.party.selection.as_ref().filter(|selection| {
            selection.accepted
                && selection.guard.accepts(&state.party)
                && selection
                    .owner
                    .as_ref()
                    .zip(state.modal_item.as_ref())
                    .is_some_and(|(owner, modal)| {
                        Arc::ptr_eq(owner, &modal.open_token)
                            && selection.request_id == modal.request_id
                    })
        });
        let guard = selection.map(|selection| selection.guard.clone());
        drop(state);
        if let Some(guard) = guard {
            Ok(Some(guard))
        } else {
            self.party_selection_error(true);
            Err(())
        }
    }

    pub(super) fn party_next_episode(&self, series: &str, episode: &str) {
        let Some(current) = self.current_playback_identity() else {
            return;
        };
        let Some(local) = self.party_modal_media(&current.episode_id) else {
            return;
        };
        let Some(next) = self.party_modal_media(episode) else {
            return;
        };
        let captured = {
            let state = self.shared.lock().unwrap();
            let party = &state.party;
            if party
                .selection
                .as_ref()
                .is_some_and(|selection| selection.next.is_some())
            {
                return;
            }
            let Some(room) = party.snapshot_for_sources().filter(can_control) else {
                drop(state);
                self.party_selection_error(false);
                return;
            };
            if !state
                .playback_intent
                .as_ref()
                .is_some_and(|intent| intent.party_bound_to(party))
            {
                drop(state);
                self.party_selection_error(true);
                return;
            }
            let Some(expected) = room.media.as_ref().filter(|_| room.pending_media.is_none())
            else {
                drop(state);
                self.party_selection_error(true);
                return;
            };
            if !compatible_media(&local, expected, party.choosing_source) {
                drop(state);
                self.party_selection_error(true);
                return;
            }
            let Some(playing) = state.playback.as_ref().filter(|playing| {
                playing.session != 0
                    && playing.series_id == series
                    && playing.episode_id == current.episode_id
            }) else {
                return;
            };
            let Some(modal) = state
                .modal_item
                .as_ref()
                .filter(|modal| modal.id == series && modal.request_id == current.episode_id)
            else {
                return;
            };
            if !next_episode::successor(&modal.videos, &current.episode_id, today_days())
                .is_some_and(|next| next.id == episode)
            {
                return;
            }
            (
                PlaybackGuard {
                    activation: party.generation,
                    room_id: room.room_id.clone(),
                    generation: room.generation,
                    media: expected.identity.clone(),
                },
                NextSelection {
                    owner: modal.open_token.clone(),
                    local_session: playing.session,
                    native_session: self.player.screenshot_session(),
                    series: series.into(),
                    current: current.episode_id,
                    episode: episode.into(),
                },
            )
        };
        if !self.party_preflight_media_inner(
            next,
            &local.identity.media_type,
            series,
            Some(&captured.0),
        ) {
            return;
        }
        // The worker's completion returns to this UI thread, so this owner is
        // installed before any accepted proposal can finish navigation.
        if let Some(selection) = self.shared.lock().unwrap().party.selection.as_mut() {
            selection.next = Some(captured.1);
        }
        if let Some(app) = self.app() {
            app.set_party_busy(true);
        }
    }

    fn party_complete_next(&self, next: NextSelection, guard: PlaybackGuard) {
        let Some(app) = self.app() else {
            return;
        };
        let current = {
            let state = self.shared.lock().unwrap();
            guard.accepts(&state.party)
                && state.playback.as_ref().is_some_and(|playing| {
                    playing.session == next.local_session
                        && playing.series_id == next.series
                        && playing.episode_id == next.current
                })
                && state.modal_item.as_ref().is_some_and(|modal| {
                    Arc::ptr_eq(&modal.open_token, &next.owner)
                        && modal.id == next.series
                        && modal.request_id == next.current
                })
        };
        if !current
            || self.player.screenshot_session() != next.native_session
            || !app.get_player_open()
        {
            return;
        }
        app.set_party_busy(false);
        // The accepted generation already replaced the room. Closing this one
        // local source must not emit the creator's global ClearMedia action.
        self.note_player_progress_from_ui();
        self.player.with_preserved_fullscreen_on_close(|| {
            let _ = self.execute_playback_action(PlaybackAction::Stop);
        });
        app.set_party_page_open(false);
        self.release_catalog_detail_for_native_successor();
        self.episode_streams_by_id(&next.series, &next.episode);
    }

    pub(super) fn party_cancel_pending_source(&self) {
        let cancelled_next = {
            let mut state = self.shared.lock().unwrap();
            if let Some(selection) = state.party.selection.as_mut() {
                selection.download = None;
                selection.next.take().is_some()
            } else {
                false
            }
        };
        if cancelled_next && let Some(app) = self.app() {
            app.set_party_busy(false);
        }
    }

    pub(super) fn party_download_guard(
        &self,
        media: PartyMedia,
        id: &str,
        kind: &str,
        title: &str,
    ) -> Result<Option<PlaybackGuard>, ()> {
        if !self.party_active() {
            return Ok(None);
        }
        if !self.party_preflight_media_for_title(media, kind, title) {
            return Err(());
        }
        let mut state = self.shared.lock().unwrap();
        let accepted =
            state.party.selection.as_ref().is_some_and(|selection| {
                selection.accepted && selection.guard.accepts(&state.party)
            });
        let Some(selection) = state.party.selection.as_mut() else {
            return Err(());
        };
        if !accepted {
            // A download card can be the first selection too. Wait for room
            // acceptance instead of loading a file under the old generation.
            selection.download = Some(id.into());
            return Err(());
        }
        let guard = selection.guard.clone();
        drop(state);
        // A download row can still belong to the retained source picker.
        // Keep manual source replacement bound to that opening when it does.
        self.party_bind_picker();
        Ok(Some(guard))
    }

    /// Explicit entry retained for coordinator callers, never passive prefetch.
    pub(super) fn party_prepare_stream_selection(&self) {
        let request_id = self
            .shared
            .lock()
            .unwrap()
            .modal_item
            .as_ref()
            .map(|m| m.request_id.clone());
        if let Some(request_id) = request_id
            && self.party_preflight_modal(&request_id)
        {
            self.party_bind_picker();
        }
    }

    fn party_follow_selection(&self, room: Option<&RoomSnapshot>) {
        let Some(room) =
            room.filter(|room| room.connected && !room.ended && room.selected_media().is_some())
        else {
            return;
        };
        {
            let mut state = self.shared.lock().unwrap();
            if state.party.followed_generation == Some(room.generation) {
                return;
            }
            // Claim once before navigation; late replies and ordinary room
            // reports must not reopen a picker the guest deliberately left.
            state.party.followed_generation = Some(room.generation);
            // The proposer already owns its matching picker. Never replace
            // its opening token or reopen a picker it deliberately dismissed.
            if state.party.selection.as_ref().is_some_and(|selection| {
                selection.guard.generation == room.generation
                    && selection.guard.room_id == room.room_id
                    && selection.accepted
            }) {
                return;
            }
        }
        self.party_choose_stream();
    }

    fn party_choose_stream(&self) {
        let media = {
            let mut state = self.shared.lock().unwrap();
            let Some(media) = state
                .party
                .snapshot
                .as_ref()
                .and_then(RoomSnapshot::selected_media)
                .cloned()
            else {
                return;
            };
            state.party.choosing_source = true;
            state.party.choosing_media = false;
            media
        };
        let owner = profiles::ProfileStore::capture();
        if !profile_policy::allows_browse_title(
            &owner,
            &media.identity.media_type,
            &media.identity.title_id,
        ) {
            return;
        }
        if self.app().is_some_and(|app| app.get_player_open()) {
            self.player.with_preserved_fullscreen_on_close(|| {
                let _ = self.execute_playback_action(PlaybackAction::Stop);
            });
        }
        if let Some(app) = self.app() {
            app.set_party_page_open(false);
        }
        self.open_preview_inner(
            MetaPreview {
                id: media.identity.title_id.clone(),
                type_: media.identity.media_type.clone(),
                name: media.identity.title.clone(),
                ..Default::default()
            },
            0,
            false,
            None,
            false,
            true,
        );
        if media.identity.media_type == "movie" {
            self.watch_now();
        } else {
            let video = {
                let mut state = self.shared.lock().unwrap();
                let Some(modal) = state.modal_item.as_mut().filter(|modal| {
                    modal.id == media.identity.title_id && modal.type_ == media.identity.media_type
                }) else {
                    return;
                };
                if let Some(video) = super::remote_playback::remote_episode(
                    &modal.videos,
                    &media.identity.episode_id,
                ) {
                    video.clone()
                } else {
                    let video = Video {
                        id: media.identity.episode_id.clone(),
                        name: media.identity.episode.clone(),
                        season: media.season,
                        episode: media.episode,
                        ..Default::default()
                    };
                    modal.videos.push(video.clone());
                    video
                }
            };
            self.select_episode_streams(media.identity.title_id, video.id.clone(), video, 0);
        }
        if let Some(app) = self.app() {
            app.set_detail_deep_stream(true);
        }
    }

    fn party_browse(&self, title: Option<&str>) {
        // This is a local source replacement, not Leave/End party.
        if self.app().is_some_and(|app| app.get_player_open()) {
            let _ = self.execute_playback_action(PlaybackAction::Stop);
        }
        if let Some(app) = self.app() {
            app.set_party_page_open(false);
            if title.is_some() {
                app.set_modal_visible(false);
                app.set_stream_selector_open(false);
                // A guest looking for the selected title gets a prepared
                // search. Hosts choose a new title manually from their current
                // page, retaining Library, Downloads and browsing filters.
                app.invoke_navigate_section(1);
            }
        }
        if let Some(title) = title {
            self.submit_search(title);
        }
    }

    fn party_prepare_media_change(&self, browse: bool) {
        if browse {
            let permitted = self
                .shared
                .lock()
                .unwrap()
                .party
                .snapshot
                .as_ref()
                .is_some_and(can_control);
            if !permitted {
                self.party_selection_error(false);
                return;
            }
            // Opening the catalog is not media intent. Cancelling browsing
            // keeps the accepted room selection and every participant intact.
            self.party_browse(None);
            return;
        }
        {
            let mut state = self.shared.lock().unwrap();
            let party = &mut state.party;
            if party.service.is_none()
                || !party
                    .snapshot
                    .as_ref()
                    .is_some_and(|r| r.role == PartyRole::Host)
            {
                return;
            }
            party.choosing_media = true;
            party.selection_revision = party.selection_revision.wrapping_add(1);
            party.source_choice = None;
            party.selection_epoch = self.player.screenshot_session();
            party.choosing_source = false;
            // Retire controls and reports for the old title before returning
            // the room to its lobby. Membership and the invite stay intact.
            party.pending.retain(|op| {
                matches!(
                    op,
                    Operation::Kick(_) | Operation::Wait(_) | Operation::Policy(_)
                )
            });
            party.pending.push_front(Operation::ClearMedia { browse });
        }
        self.player.set_paused(true);
        if let Some(app) = self.app() {
            app.set_party_busy(true);
            app.set_party_error("".into());
        }
        self.party_tick();
    }

    fn party_command(&self, command: HostCommand) {
        let rate_change = matches!(
            command,
            HostCommand::SetRate(_) | HostCommand::StepRate(_) | HostCommand::ResetRate
        );
        let (generation, revision) = {
            let state = self.shared.lock().unwrap();
            if state.party.choosing_media && !rate_change {
                return;
            }
            let Some(room) = state
                .party
                .snapshot_for_sources()
                .filter(|room| rate_change || room.media.is_some())
            else {
                return;
            };
            (room.generation, room.control_revision)
        };
        // Rate edits are room settings even in the lobby; the host resolves
        // steps against its accepted target, never this device's correction.
        self.party_enqueue(Operation::Command(generation, revision, command));
    }

    fn party_enqueue(&self, operation: Operation) {
        if !remote_playback::is_foreground() {
            return;
        }
        let mut state = self.shared.lock().unwrap();
        let p = &mut state.party;
        if p.service.is_none() {
            return;
        }
        let shared_control = matches!(operation, Operation::Command(..));
        if !p.snapshot_for_sources().as_ref().is_some_and(|room| {
            if shared_control {
                can_control(room)
            } else {
                room.role == PartyRole::Host
            }
        }) {
            drop(state);
            if let Some(app) = self.app() {
                app.set_party_error(text::tr("Only the host controls the shared timeline.").into());
            }
            return;
        }
        p.push_operation(operation);
        drop(state);
        self.party_tick();
    }

    /// Some local controls affect everyone; track, volume, screenshot and window
    /// actions deliberately continue down the ordinary local player path.
    pub(super) fn party_action(&self, action: &PlaybackAction) -> bool {
        if !self.party_active() {
            return false;
        }
        let command = match action {
            PlaybackAction::TogglePause => Some(HostCommand::SetPaused(
                !self
                    .shared
                    .lock()
                    .unwrap()
                    .party
                    .snapshot
                    .as_ref()
                    .is_none_or(|room| room.timeline.paused),
            )),
            PlaybackAction::SetPaused(paused) => Some(HostCommand::SetPaused(*paused)),
            PlaybackAction::SeekAbsolute(position) if position.is_finite() => {
                Some(HostCommand::SeekAbsolute(*position))
            }
            PlaybackAction::SeekRelative(delta) if delta.is_finite() => {
                Some(HostCommand::SeekRelative(*delta))
            }
            PlaybackAction::SetSpeed(rate) if rate.is_finite() => Some(HostCommand::SetRate(*rate)),
            PlaybackAction::SpeedStep(direction) => Some(HostCommand::StepRate(*direction)),
            PlaybackAction::ResetSpeed => Some(HostCommand::ResetRate),
            PlaybackAction::BeginSpeedPreview | PlaybackAction::EndSpeedPreview => {
                // Press-and-hold preview remains disabled in a party. Neither
                // its press nor its release may edit shared or saved speed.
                return true;
            }
            PlaybackAction::Stop => {
                let host = self
                    .shared
                    .lock()
                    .unwrap()
                    .party
                    .snapshot
                    .as_ref()
                    .is_some_and(|r| r.role == PartyRole::Host);
                if host {
                    self.party_prepare_media_change(false);
                }
                if let Some(app) = self.app() {
                    app.set_party_page_open(true);
                }
                return false;
            }
            PlaybackAction::NextEpisode { .. } => {
                if self
                    .shared
                    .lock()
                    .unwrap()
                    .party
                    .snapshot_for_sources()
                    .as_ref()
                    .is_some_and(can_control)
                {
                    return false;
                }
                self.party_selection_error(false);
                return true;
            }
            _ => return false,
        };
        if let Some(command) = command {
            self.party_command(command);
        }
        true
    }

    #[cfg(target_os = "android")]
    pub(super) fn party_suspend(&self) {
        if !self.party_active() {
            return;
        }
        self.player.set_paused(true);
        let service = {
            let mut state = self.shared.lock().unwrap();
            let p = &mut state.party;
            p.resume_guard = true;
            p.pending.retain(|operation| {
                matches!(
                    operation,
                    Operation::ClearMedia { .. }
                        | Operation::PrepareMedia(..)
                        | Operation::SelectMedia(..)
                )
            });
            if p.snapshot
                .as_ref()
                .is_some_and(|s| s.role == PartyRole::Host && s.media.is_some())
            {
                p.pending.push_front(Operation::Command(
                    p.snapshot.as_ref().map_or(0, |r| r.generation),
                    p.snapshot.as_ref().map_or(0, |r| r.control_revision),
                    HostCommand::SetPaused(true),
                ));
            }
            p.service.clone()
        };
        if let Some(service) = service {
            // Host operation is local-only. Guests stop heartbeats; the host
            // applies its disconnect grace without keeping Android awake.
            if service
                .snapshot()
                .is_some_and(|s| s.role == PartyRole::Host && s.media.is_some())
            {
                thread::spawn(move || {
                    if let Some(room) = service.snapshot() {
                        let _ = service
                            .command_for_generation(room.generation, HostCommand::SetPaused(true));
                    }
                });
            }
        }
    }

    pub(super) fn party_tick(&self) {
        let Some(app) = self.app() else { return };
        app.set_party_can_host(
            !self.party_active()
                && !self.remote_output_selected()
                && !app.get_remote_controls_open(),
        );
        app.set_party_can_return_to_player(app.get_player_open());
        if !remote_playback::is_foreground() {
            return;
        }
        let snapshot = {
            let mut state = self.shared.lock().unwrap();
            let p = &mut state.party;
            if p.snapshot.is_none() {
                p.snapshot = p.service.as_ref().and_then(|service| service.snapshot());
            }
            p.snapshot.clone()
        };
        // A contended nonblocking read is not a new, unbound media generation.
        if self.party_active() && snapshot.is_none() {
            return;
        }
        self.party_follow_selection(snapshot.as_ref());
        // Attach a newly chosen source only after its native duration and
        // seekability are known. Never reselect the video being replaced.
        let selecting = {
            let state = self.shared.lock().unwrap();
            state.party.choosing_media
                && !state.party.busy
                && !state.party.pending.iter().any(|op| {
                    matches!(
                        op,
                        Operation::PrepareMedia(..) | Operation::ClearMedia { .. }
                    )
                })
                && state.party.selection_epoch != self.player.screenshot_session()
                && state.party.selection.as_ref().is_some_and(|selection| {
                    selection.accepted && selection.guard.accepts(&state.party)
                })
                && snapshot
                    .as_ref()
                    .is_some_and(|r| r.role == PartyRole::Host && r.media.is_none())
        };
        if selecting {
            let mut candidate = self.party_report(None);
            if candidate.loaded && (!candidate.seekable || candidate.duration <= 0.0) {
                app.set_party_error(text::tr("Choose a seekable stream to watch together.").into());
            }
            if candidate.loaded
                && candidate.seekable
                && !candidate.ended
                && candidate.duration > 0.0
                && let Some(mut media) = self.party_media()
            {
                let mut state = self.shared.lock().unwrap();
                let Some(selection) = state.party.selection.as_ref().filter(|selection| {
                    selection.accepted && selection.guard.accepts(&state.party)
                }) else {
                    return;
                };
                let generation = selection.guard.generation;
                // Explicit private addon matching retains the shared identity;
                // the native duration still goes through creator validation.
                if let Some(expected) = snapshot.as_ref().and_then(RoomSnapshot::selected_media)
                    && compatible_media(&media, expected, state.party.choosing_source)
                {
                    bind_host_activation(&mut media, &mut candidate, expected);
                }
                state
                    .party
                    .pending
                    .push_front(Operation::SelectMedia(generation, media, candidate));
                app.set_party_busy(true);
            }
        }
        let report = self.party_report(snapshot.as_ref());
        if snapshot.as_ref().is_some_and(|r| r.media.is_none()) && app.get_player_open() {
            self.player.set_paused(true);
        }
        if let Some(room) = snapshot.as_ref()
            && report.loaded
            && !self.shared.lock().unwrap().party.resume_guard
            && !self.shared.lock().unwrap().party.choosing_media
        {
            if report.media.is_some()
                && let Some(target) = self.shared.lock().unwrap().playback.as_mut()
            {
                target.resume_done = true;
            }
            let correction =
                nova_party::drift_correction(room, &report, nova_party::local_now_ms());
            if (self.player.effective_playback_speed() as f64 - correction.rate).abs() > 0.001 {
                self.player.set_party_playback_rate(Some(correction.rate));
            }
            if self.player.is_paused() != correction.paused {
                let _ = self.execute_playback_action(PlaybackAction::SetPaused(correction.paused));
            }
            if let Some(position) = correction.seek_to {
                let now = Instant::now();
                let seek = {
                    let mut state = self.shared.lock().unwrap();
                    let p = &mut state.party;
                    let due = p.last_seek.is_none_or(|(at, old)| {
                        at.elapsed().as_secs_f32() >= 1.0 || (old - position).abs() > 1.0
                    });
                    if due {
                        p.last_seek = Some((now, position));
                    }
                    if let Some(target) = state.playback.as_mut() {
                        target.resume_done = true;
                    }
                    due
                };
                if seek {
                    let _ = self.execute_playback_action(PlaybackAction::SeekAbsolute(position));
                }
            }
        }
        let work = {
            let mut state = self.shared.lock().unwrap();
            let p = &mut state.party;
            if p.busy {
                return;
            }
            let Some(service) = p.service.clone() else {
                return;
            };
            // Keep the heartbeat deadline under a control burst. A slow
            // report gets one queued operation before another overdue report,
            // so RTT >= the deadline cannot starve controls indefinitely.
            let operation = if p.report_should_run(Instant::now()) {
                Some(Operation::Report(report))
            } else if let Some(wait) = p.guest_control_wait(Instant::now()) {
                let Some(room) = p.snapshot_for_sources() else {
                    return;
                };
                let wakeup = ControlWakeup {
                    activation: p.generation,
                    room_id: room.room_id,
                    generation: room.generation,
                };
                let schedule = p.control_wakeup.as_ref() != Some(&wakeup);
                if schedule {
                    p.control_wakeup = Some(wakeup.clone());
                }
                drop(state);
                if schedule {
                    let bridge = self.clone();
                    slint::Timer::single_shot(wait, move || {
                        let mut state = bridge.shared.lock().unwrap();
                        if state.party.control_wakeup.as_ref() != Some(&wakeup) {
                            return;
                        }
                        state.party.control_wakeup = None;
                        if !wakeup.current(&state.party) {
                            return;
                        }
                        drop(state);
                        bridge.party_tick();
                    });
                }
                return;
            } else {
                p.pending.pop_front()
            };
            let Some(operation) = operation else { return };
            let guest_control = operation.is_control()
                && p.snapshot_for_sources()
                    .is_some_and(|room| room.role == PartyRole::Guest);
            p.busy = true;
            p.note_operation_dispatched(&operation, Instant::now());
            (
                service,
                operation,
                p.generation,
                p.resume_guard,
                p.selection_revision,
                guest_control,
            )
        };
        let b = self.clone();
        thread::spawn(move || {
            let (service, operation, generation, resumed, selection_revision, guest_control) = work;
            let browse = matches!(operation, Operation::ClearMedia { browse: true });
            let media_change = matches!(
                operation,
                Operation::ClearMedia { .. }
                    | Operation::PrepareMedia(..)
                    | Operation::SelectMedia(..)
            );
            let selecting_media = matches!(operation, Operation::SelectMedia(..));
            let preparing_media = matches!(operation, Operation::PrepareMedia(..));
            let operation_media_generation = match &operation {
                Operation::PrepareMedia(expected, ..) | Operation::SelectMedia(expected, ..) => {
                    Some(*expected)
                }
                _ => None,
            };
            let report_only = matches!(operation, Operation::Report(_));
            let result = if !remote_playback::is_foreground() && report_only {
                Ok(())
            } else {
                match operation {
                    Operation::Command(media_generation, revision, command) => {
                        service.command_for_state(media_generation, revision, command)
                    }
                    Operation::ClearMedia { .. } => service.clear_media(),
                    Operation::PrepareMedia(media_generation, revision, media) => {
                        service.propose_media(media_generation, revision, media)
                    }
                    Operation::Policy(policy) => service.set_control_policy(policy),
                    Operation::SelectMedia(media_generation, media, report) => {
                        service
                            .select_media_for_generation(media_generation, media, report)
                            .and_then(|()| {
                                // A load can finish after Android suspends. A prior
                                // generation's pause must not let its replacement
                                // start playing for the other participants.
                                if !remote_playback::is_foreground() {
                                    service.command_for_generation(
                                        media_generation,
                                        HostCommand::SetPaused(true),
                                    )?;
                                }
                                Ok(())
                            })
                    }
                    Operation::Kick(id) => service.kick(&id),
                    Operation::Wait(enabled) => service.set_wait_for_guests(enabled),
                    Operation::Continue => service.continue_without_waiting(),
                    Operation::Report(report) => service.report(report),
                }
            };
            let snapshot = service.snapshot();
            let _ = crate::app::profiles::invoke_from_event_loop(move || {
                let current_media = b.current_playback_identity();
                let reopen_room;
                let mut download = None;
                let mut next = None;
                {
                    let mut state = b.shared.lock().unwrap();
                    let p = &mut state.party;
                    if p.generation != generation {
                        return;
                    }
                    if guest_control {
                        // The host's limiter includes rejected controls. Waiting
                        // after the reply also remains safe under RTT jitter.
                        p.last_guest_control_finished = Some(Instant::now());
                    }
                    if media_change && p.selection_revision != selection_revision {
                        p.busy = false;
                        drop(state);
                        b.party_tick();
                        return;
                    }
                    reopen_room = snapshot.as_ref().is_some_and(|room| {
                        p.snapshot.as_ref().map(|old| old.generation) != Some(room.generation)
                            && room.selected_media().is_none()
                    });
                    p.busy = false;
                    // Keep the selection intent while a load is pending so
                    // suspension cannot strand an otherwise ready lobby.
                    if selecting_media || (media_change && result.is_err()) {
                        p.choosing_media = false;
                    }
                    if preparing_media && result.is_err() {
                        // Rejection can retain the same generation. Retire the
                        // failed dedup marker so choosing it again can retry.
                        p.reject_prepared_selection();
                    }
                    if let Some(room) = snapshot.as_ref() {
                        let changed =
                            p.snapshot.as_ref().map(|r| r.generation) != Some(room.generation);
                        let accepted_proposal = preparing_media
                            && result.is_ok()
                            && operation_media_generation.is_some_and(|expected| {
                                p.prepared_selection_matches(room, expected)
                            });
                        let accepted_activation = selecting_media
                            && result.is_ok()
                            && operation_media_generation == Some(room.generation);
                        if accepted_proposal {
                            (download, next) = p.accept_prepared_selection(
                                room,
                                operation_media_generation.unwrap(),
                            );
                        } else if preparing_media {
                            p.reject_prepared_selection();
                            p.choosing_media = false;
                        }
                        if changed {
                            // A late rejected proposal must not clear the
                            // creator's new source-follow/activation intent.
                            p.retire_old_operations(room.generation);
                            p.last_seek = None;
                            p.choosing_source = false;
                            // A host source can finish while its episode
                            // announcement is in flight. Both host selection
                            // stages bind that same local asset to this generation.
                            if (accepted_activation || accepted_proposal)
                                && p.source_choice.as_ref().is_some_and(|choice| {
                                    current_media.as_ref().is_some_and(|media| {
                                        party_source::same_media(&choice.media, media)
                                    })
                                })
                            {
                                if let Some(choice) = p.source_choice.as_mut() {
                                    choice.generation = room.generation;
                                }
                            } else {
                                p.source_choice = None;
                            }
                        }
                        if !room.connected || room.ended {
                            p.source_choice = None;
                        }
                        p.snapshot = snapshot;
                    }
                    if resumed && result.is_ok() && remote_playback::is_foreground() {
                        p.resume_guard = false;
                    }
                }
                if let Some(app) = b.app() {
                    // Guests choose their own source for each newly selected
                    // title. EOF brings the host back to the existing lobby.
                    if reopen_room {
                        app.set_party_page_open(true);
                    }
                    if media_change {
                        app.set_party_busy(false);
                    }
                    if let Err(error) = &result {
                        app.set_party_error(
                            if let Some(rejection) =
                                error.downcast_ref::<nova_party::ControlRejection>()
                            {
                                text::tr(&rejection.to_string()).into()
                            } else {
                                text::tr("Could not connect to the watch party. Try again.").into()
                            },
                        );
                    } else if !report_only {
                        app.set_party_error("".into());
                    }
                }
                if browse && result.is_ok() {
                    b.party_browse(None);
                }
                if let Some(id) = download {
                    b.play_download(&id);
                }
                if let Some((choice, guard)) = next {
                    b.party_complete_next(choice, guard);
                }
                if remote_playback::is_foreground() {
                    b.party_publish();
                    b.party_tick();
                }
            });
        });
    }

    pub(super) fn refresh_party_language_text(&self) {
        self.party_publish();
    }

    fn party_publish(&self) {
        self.refresh_party_stream_recommendations();
        let Some(app) = self.app() else { return };
        let room = self.shared.lock().unwrap().party.snapshot.clone();
        let Some(room) = room else { return };
        app.set_party_is_host(room.role == PartyRole::Host);
        app.set_party_allow_everyone(room.control_policy == ControlPolicy::Everyone);
        app.set_party_can_control(can_control(&room));
        app.set_party_can_choose_media(can_control(&room));
        app.set_party_active(!room.ended);
        app.set_party_waiting(!room.waiting.is_empty() || !room.connected);
        app.set_party_wait_for_guests(room.wait_for_guests);
        app.set_party_paused(room.timeline.paused);
        app.set_party_target_speed(room.timeline.target_rate as f32);
        let media = room.selected_media();
        app.set_party_has_media(media.is_some());
        app.set_party_can_return_to_player(app.get_player_open());
        app.set_party_title(media.map_or("", |m| m.identity.title.as_str()).into());
        app.set_party_spoilers_hidden(media.is_some_and(|media| {
            let state = self.shared.lock().unwrap();
            spoiler::hidden(
                &state,
                &media.identity.media_type,
                &media.identity.title_id,
                &media.identity.episode_id,
            )
        }));
        let episode = media
            .map(|media| party_episode_caption(&self.shared.lock().unwrap(), media))
            .unwrap_or_default();
        app.set_party_episode(episode.into());
        let duration = media.map_or(0.0, |m| m.duration);
        let position = room
            .timeline
            .position_at(nova_party::local_now_ms() as f64 + room.clock_offset_ms)
            .clamp(0.0, duration);
        app.set_party_position(position as f32);
        app.set_party_duration(duration as f32);
        app.set_party_time_text(if duration > 0.0 {
            format!(
                "{} / {}",
                crate::player::format_time(position),
                crate::player::format_time(duration)
            )
            .into()
        } else {
            "".into()
        });
        let status = if room.ended {
            "The watch party has ended."
        } else if !room.connected {
            "Reconnecting to the host…"
        } else if room.pending_media.is_some() {
            "Choose a stream"
        } else if media.is_none() {
            if can_control(&room) {
                // The lobby already presents this action as its primary button.
                ""
            } else {
                "Waiting for the host to choose"
            }
        } else if !room.waiting.is_empty() {
            "Waiting for participants to buffer…"
        } else if room.timeline.paused {
            "Paused"
        } else {
            "Watching together"
        };
        app.set_party_status(text::tr(status).into());
        app.set_party_warning(if media.is_some() { text::tr("Choose the same episode and edition. Different cuts cannot be aligned automatically.").into() } else { "".into() });
        app.set_party_participants(
            Rc::new(VecModel::from(
                room.participants
                    .iter()
                    .map(|p| crate::PartyMemberRow {
                        id: p.id.clone().into(),
                        name: p.name.clone().into(),
                        initial: participant_initial(&p.name),
                        is_host: p.is_host,
                        ready: p.ready,
                        can_remove: room.role == PartyRole::Host && !p.is_host,
                        status: text::party_buffered_status(
                            if !p.connected {
                                "Disconnected"
                            } else if p.buffering {
                                "Buffering"
                            } else if p.ready {
                                "Ready"
                            } else {
                                match p.reason.as_deref() {
                                    Some("waiting for host to choose media") => {
                                        "Waiting for the host to choose"
                                    }
                                    Some("buffering") => "Buffering",
                                    Some("waiting for current media") => "Loading stream…",
                                    Some("different media duration or edition") => {
                                        "Different edition or duration"
                                    }
                                    Some("this source cannot seek") => "This stream cannot seek",
                                    Some("local source unavailable") => "Local stream unavailable",
                                    Some("local media finished") => "Finished",
                                    Some("choose the same title and episode") => {
                                        "Choose the same title and episode"
                                    }
                                    _ => "Choosing a stream",
                                }
                            },
                            p.buffered_ahead,
                        )
                        .into(),
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
        );
        if room.ended {
            self.player.set_paused(true);
            self.party_leave();
            app.set_party_status(text::tr("The watch party has ended.").into());
        }
        if app.get_party_page_open() {
            app.recover_back_focus();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn participant_initial_uses_a_readable_normalized_letter() {
        for (name, initial) in [
            (" Mišo ", "M"),
            ("e\u{301}va", "É"),
            ("ßara", "S"),
            ("", "?"),
        ] {
            assert_eq!(participant_initial(name), initial);
        }
    }
    #[test]
    fn spoiler_party_caption_uses_local_watched_state_and_exact_reveal() {
        let mut state = Shared::default();
        state.cache_settings.spoiler_protection = true;
        let mut first = media("spoiler-party", 1);
        first.identity.episode = "The surprise ending".into();
        assert_eq!(party_episode_caption(&state, &first), "S1 E1");
        let mut same_number = first.clone();
        same_number.identity.title_id = "unproven-provider".into();
        Bridge::set_episode_watched_locked(
            &mut state.progress,
            &same_number.identity.title_id,
            &same_number.identity.episode_id,
            true,
        );
        assert_eq!(party_episode_caption(&state, &first), "S1 E1");
        spoiler::reveal("series", "spoiler-party", &first.identity.episode_id);
        assert_eq!(party_episode_caption(&state, &first), "The surprise ending");
        let mut second = media("spoiler-party", 2);
        second.identity.episode = "A second surprise".into();
        assert_eq!(party_episode_caption(&state, &second), "S1 E2");
        Bridge::set_episode_watched_locked(
            &mut state.progress,
            "spoiler-party",
            &second.identity.episode_id,
            true,
        );
        assert_eq!(party_episode_caption(&state, &second), "A second surprise");
        Bridge::set_episode_watched_locked(
            &mut state.progress,
            "spoiler-party",
            &second.identity.episode_id,
            false,
        );
        assert_eq!(party_episode_caption(&state, &second), "S1 E2");
        second.season = None;
        second.episode = None;
        assert_eq!(party_episode_caption(&state, &second), "Episode");
    }

    #[test]
    fn private_host_source_activation_binds_media_and_report_together() {
        let expected = media("tt123", 2);
        let mut local = media("private-addon:blue-box", 2);
        assert!(compatible_media(&local, &expected, true));
        let mut report = LocalReport {
            media: Some(local.identity.clone()),
            loaded: true,
            seekable: true,
            duration: 1_400.0,
            ..Default::default()
        };
        bind_host_activation(&mut local, &mut report, &expected);
        assert_eq!(local.identity, expected.identity);
        assert_eq!(report.media.as_ref(), Some(&local.identity));
        assert_eq!(report.duration, 1_400.0);
    }

    #[test]
    fn room_change_retires_old_controls_without_losing_new_activation_or_admin_operations() {
        let mut state = State::default();
        state.pending.extend([
            Operation::PrepareMedia(1, 0, media("tt123", 1)),
            Operation::SelectMedia(1, media("tt123", 1), LocalReport::default()),
            Operation::Command(1, 0, HostCommand::SeekAbsolute(50.0)),
            Operation::Report(LocalReport {
                generation: 1,
                ..Default::default()
            }),
            Operation::SelectMedia(2, media("tt123", 2), LocalReport::default()),
            Operation::Command(2, 0, HostCommand::SetPaused(true)),
            Operation::Kick("guest".into()),
            Operation::Wait(false),
            Operation::Policy(ControlPolicy::HostOnly),
        ]);
        state.retire_old_operations(2);
        assert_eq!(state.pending.len(), 5);
        assert!(matches!(state.pending[0], Operation::SelectMedia(2, ..)));
        assert!(matches!(state.pending[1], Operation::Command(2, ..)));
        assert!(matches!(state.pending[2], Operation::Kick(_)));
        assert!(matches!(state.pending[3], Operation::Wait(false)));
        assert!(matches!(state.pending[4], Operation::Policy(_)));
    }

    fn media(id: &str, episode: u32) -> PartyMedia {
        PartyMedia {
            identity: MediaIdentity {
                media_type: "series".into(),
                title_id: id.into(),
                episode_id: format!("{id}:1:{episode}"),
                title: "Blue Box".into(),
                episode: format!("S1 E{episode}"),
            },
            season: Some(1),
            episode: Some(episode),
            duration: 1_400.0,
        }
    }
    #[test]
    fn matching_requires_identity_or_explicit_private_source_choice() {
        assert!(compatible_media(
            &media("tt123", 2),
            &media("tt123", 2),
            false
        ));
        assert!(!compatible_media(
            &media("tt123", 1),
            &media("tt123", 2),
            true
        ));
        assert!(!compatible_media(
            &media("tt456", 2),
            &media("tt123", 2),
            true
        ));
        assert!(!compatible_media(
            &media("anikoto:box", 2),
            &media("tt123", 2),
            false
        ));
        assert!(compatible_media(
            &media("anikoto:box", 2),
            &media("tt123", 2),
            true
        ));
        assert!(!compatible_media(
            &media("anikoto:box", 1),
            &media("tt123", 2),
            true
        ));
    }
}

#[cfg(all(test, feature = "desktop"))]
#[path = "party_tests.rs"]
mod integration_tests;

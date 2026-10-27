//! A temporary party shares a timeline, never a stream or a sync relationship.
//! The UI thread owns player access. One worker operation at a time owns all
//! room I/O, while local drift corrections use the latest immutable snapshot.
use super::*;
use nova_party::{HostCommand, LocalReport, PartyMedia, PartyRole, PartyService, RoomSnapshot};
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
    /// A user explicitly chose to find this room's episode in their own addons.
    choosing_source: bool,
    /// Only an explicit host selection may replace the shared title.
    choosing_media: bool,
    selection_epoch: u64,
    selection_revision: u64,
    followed_generation: Option<u64>,
    last_seek: Option<(Instant, f64)>,
    resume_guard: bool,
    setup_restore: Option<(u64, bool)>,
    source_choice: Option<SourceChoice>,
}

#[derive(Clone)]
struct SourceChoice {
    activation: u64,
    generation: u64,
    media: nova_playback_api::MediaIdentity,
    fingerprint: nova_party::SourceFingerprint,
}

impl State {
    pub(super) fn snapshot_for_sources(&self) -> Option<RoomSnapshot> {
        self.snapshot.clone()
    }
    pub(super) fn active(&self) -> bool {
        self.service.is_some() || self.busy
    }
}

enum Operation {
    Command(u64, HostCommand),
    ClearMedia { browse: bool },
    PrepareMedia(PartyMedia),
    SelectMedia(PartyMedia, LocalReport),
    Kick(String),
    Wait(bool),
    Continue,
    Report(LocalReport),
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
        if room.role == PartyRole::Host && self.shared.lock().unwrap().party.choosing_media {
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
                    service.create_lobby(name)?
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

    /// Stream-picker entry publishes identity before any host source is loaded.
    pub(super) fn party_prepare_stream_selection(&self) {
        if profile_policy::entry_blocked() {
            return;
        }
        let media = {
            let state = self.shared.lock().unwrap();
            let party = &state.party;
            if !party
                .snapshot
                .as_ref()
                .is_some_and(|room| room.role == PartyRole::Host)
                || state.season_download.is_some()
                || !self.app().is_some_and(|app| app.get_stream_selector_open())
            {
                return;
            }
            let Some(modal) = state.modal_item.as_ref() else {
                return;
            };
            let video = modal
                .videos
                .iter()
                .find(|video| video.id == modal.request_id);
            if modal.type_ != "movie" && video.is_none() {
                return;
            }
            let identity = party_source::canonical_media(
                nova_playback_api::MediaIdentity {
                    media_type: modal.type_.clone(),
                    title_id: modal.id.clone(),
                    episode_id: modal.request_id.clone(),
                    title: modal.name.clone(),
                    episode: video.map(|video| video.name.clone()).unwrap_or_default(),
                },
                &modal.videos,
            );
            if party.snapshot.as_ref().and_then(RoomSnapshot::selected_media)
                .is_some_and(|media| party_source::same_media(&media.identity, &identity))
                || party.pending.iter().any(|operation| matches!(operation, Operation::PrepareMedia(media) if party_source::same_media(&media.identity, &identity)))
            { return; }
            PartyMedia {
                identity,
                season: video.and_then(|v| v.season),
                episode: video.and_then(|v| v.episode.or(v.number)),
                duration: 0.0,
            }
        };
        {
            let mut state = self.shared.lock().unwrap();
            let party = &mut state.party;
            party.choosing_media = true;
            party.choosing_source = false;
            party.source_choice = None;
            party.selection_epoch = self.player.screenshot_session();
            party.selection_revision = party.selection_revision.wrapping_add(1);
            party
                .pending
                .retain(|op| matches!(op, Operation::Kick(_) | Operation::Wait(_)));
            party.pending.push_front(Operation::PrepareMedia(media));
        }
        self.party_tick();
    }

    fn party_follow_selection(&self, room: Option<&RoomSnapshot>) {
        let Some(room) = room.filter(|room| {
            room.role == PartyRole::Guest
                && room.connected
                && !room.ended
                && room.selected_media().is_some()
        }) else {
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
            let _ = self.execute_playback_action(PlaybackAction::Stop);
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
            party
                .pending
                .retain(|op| matches!(op, Operation::Kick(_) | Operation::Wait(_)));
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
        let generation = {
            let state = self.shared.lock().unwrap();
            if state.party.choosing_media {
                return;
            }
            let Some(room) = state.party.snapshot.as_ref().filter(|r| r.media.is_some()) else {
                return;
            };
            room.generation
        };
        self.party_enqueue(Operation::Command(generation, command));
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
        if !p
            .snapshot
            .as_ref()
            .is_some_and(|room| room.role == PartyRole::Host)
        {
            drop(state);
            if let Some(app) = self.app() {
                app.set_party_error(text::tr("Only the host controls the shared timeline.").into());
            }
            return;
        }
        if p.pending.len() < 32 {
            p.pending.push_back(operation);
        }
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
            PlaybackAction::SetSpeed(_)
            | PlaybackAction::SpeedStep(_)
            | PlaybackAction::ResetSpeed
            | PlaybackAction::BeginSpeedPreview
            | PlaybackAction::EndSpeedPreview => {
                if let Some(app) = self.app() {
                    app.set_party_warning(text::tr("Watch parties use normal speed; audio and subtitles remain your choice.").into());
                }
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
                    .snapshot
                    .as_ref()
                    .is_some_and(|r| r.role == PartyRole::Host)
                {
                    return false;
                }
                if let Some(app) = self.app() {
                    app.set_party_error(
                        text::tr("Only the host controls the shared timeline.").into(),
                    );
                }
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
                    Operation::ClearMedia { .. } | Operation::SelectMedia(..)
                )
            });
            if p.snapshot
                .as_ref()
                .is_some_and(|s| s.role == PartyRole::Host && s.media.is_some())
            {
                p.pending.push_front(Operation::Command(
                    p.snapshot.as_ref().map_or(0, |r| r.generation),
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
                        Operation::PrepareMedia(_) | Operation::ClearMedia { .. }
                    )
                })
                && state.party.selection_epoch != self.player.screenshot_session()
                && snapshot
                    .as_ref()
                    .is_some_and(|r| r.role == PartyRole::Host && r.media.is_none())
        };
        if selecting {
            let candidate = self.party_report(None);
            if candidate.loaded && (!candidate.seekable || candidate.duration <= 0.0) {
                app.set_party_error(text::tr("Choose a seekable stream to watch together.").into());
            }
            if candidate.loaded
                && candidate.seekable
                && !candidate.ended
                && candidate.duration > 0.0
                && let Some(media) = self.party_media()
            {
                let mut state = self.shared.lock().unwrap();
                state
                    .party
                    .pending
                    .push_front(Operation::SelectMedia(media, candidate));
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
            let operation = p.pending.pop_front().or_else(|| {
                p.last_report
                    .is_none_or(|at| at.elapsed().as_millis() >= 500)
                    .then_some(Operation::Report(report))
            });
            let Some(operation) = operation else { return };
            p.busy = true;
            p.last_report = Some(Instant::now());
            (
                service,
                operation,
                p.generation,
                p.resume_guard,
                p.selection_revision,
            )
        };
        let b = self.clone();
        thread::spawn(move || {
            let (service, operation, generation, resumed, selection_revision) = work;
            let browse = matches!(operation, Operation::ClearMedia { browse: true });
            let media_change = matches!(
                operation,
                Operation::ClearMedia { .. }
                    | Operation::PrepareMedia(_)
                    | Operation::SelectMedia(..)
            );
            let selecting_media = matches!(operation, Operation::SelectMedia(..));
            let preparing_media = matches!(operation, Operation::PrepareMedia(_));
            let report_only = matches!(operation, Operation::Report(_));
            let result = if !remote_playback::is_foreground() && report_only {
                Ok(())
            } else {
                match operation {
                    Operation::Command(media_generation, command) => {
                        service.command_for_generation(media_generation, command)
                    }
                    Operation::ClearMedia { .. } => service.clear_media(),
                    Operation::PrepareMedia(media) => service.prepare_media(media),
                    Operation::SelectMedia(media, report) => {
                        service.select_media(media, report).and_then(|()| {
                            // A load can finish after Android suspends. A prior
                            // generation's pause must not let its replacement
                            // start playing for the other participants.
                            if !remote_playback::is_foreground()
                                && let Some(room) = service.snapshot()
                            {
                                service.command_for_generation(
                                    room.generation,
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
                {
                    let mut state = b.shared.lock().unwrap();
                    let p = &mut state.party;
                    if p.generation != generation {
                        return;
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
                    if let Some(room) = snapshot.as_ref() {
                        let changed =
                            p.snapshot.as_ref().map(|r| r.generation) != Some(room.generation);
                        if changed {
                            p.last_seek = None;
                            p.choosing_source = false;
                            // A host source can finish while its episode
                            // announcement is in flight. Both host selection
                            // stages bind that same local asset to this generation.
                            if (selecting_media || preparing_media)
                                && result.is_ok()
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
                    if result.is_err() {
                        app.set_party_error(
                            text::tr("Could not connect to the watch party. Try again.").into(),
                        );
                    } else if !report_only {
                        app.set_party_error("".into());
                    }
                }
                if browse && result.is_ok() {
                    b.party_browse(None);
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
        app.set_party_active(!room.ended);
        app.set_party_waiting(!room.waiting.is_empty() || !room.connected);
        app.set_party_wait_for_guests(room.wait_for_guests);
        app.set_party_paused(room.timeline.paused);
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
            if room.role == PartyRole::Host {
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

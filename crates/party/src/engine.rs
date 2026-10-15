//! Deterministic host authority and wait-together state machine.
use crate::*;
use anyhow::{Result, ensure};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const DISCONNECTED_MS: u64 = 2_000;
const DISCONNECT_GRACE_MS: u64 = 10_000;
const STALL_DEBOUNCE_MS: u64 = 800;
const RECOVER_MS: u64 = 500;
pub(crate) const ROOM_LIFETIME_MS: u64 = 4 * 60 * 60 * 1_000;
pub(crate) const INVITE_LIFETIME_MS: u64 = 60 * 60 * 1_000;
const MAX_ADMISSIONS: usize = 64;

pub(crate) struct Member {
    pub participant: Participant,
    report: LocalReport,
    last_seen: u64,
    pub last_sequence: u64,
    admitted: bool,
    stalled: bool,
    bad_since: Option<u64>,
    recovered_since: Option<u64>,
}

pub(crate) struct HostRoom {
    pub snapshot: RoomSnapshot,
    pub capability: [u8; 32],
    pub created: u64,
    pub members: BTreeMap<String, Member>,
    revoked: BTreeSet<String>,
    intended_paused: bool,
    held: bool,
    admissions: usize,
}

pub(crate) fn media_matches(expected: &MediaIdentity, actual: Option<&MediaIdentity>) -> bool {
    actual.is_some_and(|actual| {
        !expected.title_id.is_empty()
            && actual.media_type == expected.media_type
            && actual.title_id == expected.title_id
            && actual.episode_id == expected.episode_id
    })
}

pub(crate) fn duration_matches(expected: f64, actual: f64) -> bool {
    actual.is_finite() && actual > 0.0 && (expected - actual).abs() <= 3.0
}

pub(crate) fn valid_identity(media: &MediaIdentity) -> bool {
    [
        &media.media_type,
        &media.title_id,
        &media.episode_id,
        &media.title,
        &media.episode,
    ]
    .iter()
    .all(|s| s.len() <= 256 && !s.contains('\0'))
        && !media.title_id.is_empty()
        && !media.title_id.contains("://")
        && !media.episode_id.contains("://")
}

pub(crate) fn validate_media(media: &PartyMedia) -> Result<()> {
    ensure!(
        valid_identity(&media.identity),
        "invalid party media identity"
    );
    ensure!(
        media.duration.is_finite() && media.duration > 0.0 && media.duration <= 604_800.0,
        "a finite, seekable media duration is required"
    );
    Ok(())
}

pub(crate) fn validate_report(report: &LocalReport) -> Result<()> {
    ensure!(
        [report.position, report.duration, report.buffered_ahead]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0 && *v <= 604_800.0),
        "invalid player report"
    );
    ensure!(
        report.error.as_ref().is_none_or(|s| s.len() <= 256),
        "player error too long"
    );
    ensure!(
        report.media.as_ref().is_none_or(valid_identity),
        "invalid media identity"
    );
    Ok(())
}

fn reason(media: &PartyMedia, generation: u64, report: &LocalReport) -> Option<String> {
    if report.generation != generation {
        return Some("waiting for current media".into());
    }
    if !media_matches(&media.identity, report.media.as_ref()) {
        return Some("choose the same title and episode".into());
    }
    if report.error.is_some() {
        // Provider/mpv errors may contain a source URL or credentials. Publish
        // only readiness, never the local diagnostic text.
        return Some("local source unavailable".into());
    }
    if report.ended {
        return Some("local media finished".into());
    }
    if !report.loaded {
        return Some("choose a local stream".into());
    }
    if !report.seekable {
        return Some("this source cannot seek".into());
    }
    if !duration_matches(media.duration, report.duration) {
        return Some("different media duration or edition".into());
    }
    if report.buffering
        || !report.ready
        || report.buffered_ahead + 0.05
            < BUFFER_READY_SECONDS.min((report.duration - report.position).max(0.0))
    {
        return Some("buffering".into());
    }
    None
}

pub(crate) fn ready_for_media(media: &PartyMedia, generation: u64, report: &LocalReport) -> bool {
    reason(media, generation, report).is_none()
}

impl Member {
    fn new(id: String, name: String, is_host: bool, now: u64) -> Self {
        Self {
            participant: Participant {
                id,
                name,
                is_host,
                connected: true,
                ready: false,
                buffering: false,
                position: 0.0,
                buffered_ahead: 0.0,
                reason: None,
            },
            report: LocalReport::default(),
            last_seen: now,
            last_sequence: 0,
            admitted: is_host,
            stalled: false,
            bad_since: None,
            recovered_since: None,
        }
    }
}

impl HostRoom {
    pub fn new(
        room_id: String,
        host_id: String,
        capability: [u8; 32],
        media: PartyMedia,
        report: LocalReport,
        name: String,
        now: u64,
    ) -> Self {
        Self::with_media(room_id, host_id, capability, Some(media), report, name, now)
    }

    pub fn lobby(
        room_id: String,
        host_id: String,
        capability: [u8; 32],
        name: String,
        now: u64,
    ) -> Self {
        Self::with_media(
            room_id,
            host_id,
            capability,
            None,
            LocalReport::default(),
            name,
            now,
        )
    }

    fn with_media(
        room_id: String,
        host_id: String,
        capability: [u8; 32],
        media: Option<PartyMedia>,
        report: LocalReport,
        name: String,
        now: u64,
    ) -> Self {
        let paused = media.is_none();
        let mut host = Member::new(host_id.clone(), name, true, now);
        host.report = report.clone();
        let mut room = Self {
            snapshot: RoomSnapshot {
                role: PartyRole::Host,
                room_id,
                host_id: host_id.clone(),
                member_id: host_id.clone(),
                media,
                generation: 1,
                revision: 1,
                timeline: Timeline {
                    position: report.position,
                    anchor_ms: now + START_LEAD_MS,
                    paused,
                },
                participants: Vec::new(),
                waiting: Vec::new(),
                wait_for_guests: true,
                connected: true,
                ended: false,
                error: None,
                clock_offset_ms: 0.0,
            },
            capability,
            created: now,
            members: BTreeMap::from([(host_id, host)]),
            revoked: BTreeSet::new(),
            intended_paused: false,
            held: false,
            admissions: 1,
        };
        room.tick(now);
        room
    }

    /// Authentication is checked after reading every frame, while holding the
    /// same lock as kick/leave. Queued work cannot cross a revocation.
    pub fn authorize(
        &self,
        room_id: &str,
        capability: &[u8; 32],
        peer: &str,
        now: u64,
    ) -> Result<()> {
        let different = self
            .capability
            .iter()
            .zip(capability)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b));
        ensure!(
            different == 0 && room_id == self.snapshot.room_id,
            "invalid party capability"
        );
        ensure!(
            (!self.snapshot.ended || self.members.contains_key(peer))
                && now.saturating_sub(self.created) < ROOM_LIFETIME_MS,
            "party has ended"
        );
        ensure!(!self.revoked.contains(peer), "party membership revoked");
        Ok(())
    }

    pub fn join(&mut self, id: &str, name: String, sequence: u64, now: u64) -> Result<()> {
        ensure!(!self.snapshot.ended, "party has ended");
        ensure!(id != self.snapshot.host_id, "host cannot join itself");
        if let Some(member) = self.members.get_mut(id) {
            if sequence > member.last_sequence {
                member.last_sequence = sequence;
                member.last_seen = now;
            }
            return Ok(());
        }
        ensure!(
            now.saturating_sub(self.created) < INVITE_LIFETIME_MS,
            "invite expired"
        );
        ensure!(
            self.members.len() < MAX_MEMBERS && self.admissions < MAX_ADMISSIONS,
            "party is full"
        );
        let mut member = Member::new(id.into(), name, false, now);
        member.last_sequence = sequence;
        self.members.insert(id.into(), member);
        self.admissions += 1;
        self.tick(now);
        Ok(())
    }

    pub fn report(&mut self, id: &str, report: LocalReport, sequence: u64, now: u64) -> Result<()> {
        validate_report(&report)?;
        let member = self
            .members
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("join the party first"))?;
        if sequence <= member.last_sequence {
            return Ok(());
        }
        ensure!(
            now.saturating_sub(member.last_seen) >= 50 || member.last_sequence == 0,
            "party reports too frequent"
        );
        member.last_sequence = sequence;
        member.last_seen = now;
        // Out-of-order reports must not replace readiness for the active media.
        if report.generation == self.snapshot.generation && self.snapshot.media.is_some() {
            member.report = report;
        }
        self.tick(now);
        Ok(())
    }

    pub fn local_report(&mut self, report: LocalReport, now: u64) -> Result<()> {
        validate_report(&report)?;
        if now.saturating_sub(self.members[&self.snapshot.host_id].last_seen) >= HOST_TIMEOUT_MS {
            self.snapshot.ended = true;
            self.snapshot.error = Some("host disconnected".into());
        }
        // Reports from a previous load can still be queued while the host
        // chooses another episode. They keep the foreground heartbeat alive,
        // but cannot finish or change readiness for the current selection.
        let current = report.generation == self.snapshot.generation;
        if current
            && report.ended
            && self
                .snapshot
                .media
                .as_ref()
                .is_some_and(|media| media_matches(&media.identity, report.media.as_ref()))
            && !self.snapshot.ended
        {
            self.reset_media(None, LocalReport::default(), now)?;
        }
        let host = self
            .members
            .get_mut(&self.snapshot.host_id)
            .expect("host exists");
        if report.generation == self.snapshot.generation && self.snapshot.media.is_some() {
            host.report = report;
        }
        host.last_seen = now;
        self.tick(now);
        Ok(())
    }

    pub fn select_media(
        &mut self,
        media: PartyMedia,
        mut report: LocalReport,
        now: u64,
    ) -> Result<()> {
        validate_media(&media)?;
        validate_report(&report)?;
        ensure!(
            report.loaded && report.seekable && !report.ended,
            "load a seekable stream before hosting"
        );
        ensure!(
            duration_matches(media.duration, report.duration),
            "host media duration differs"
        );
        if report.media.is_none() {
            report.media = Some(media.identity.clone());
        }
        ensure!(
            media_matches(&media.identity, report.media.as_ref()),
            "host media differs"
        );
        self.tick(now);
        ensure!(!self.snapshot.ended, "party has ended");
        self.reset_media(Some(media), report, now)
    }

    pub fn clear_media(&mut self, now: u64) -> Result<()> {
        self.tick(now);
        ensure!(!self.snapshot.ended, "party has ended");
        self.reset_media(None, LocalReport::default(), now)
    }

    fn reset_media(
        &mut self,
        media: Option<PartyMedia>,
        mut report: LocalReport,
        now: u64,
    ) -> Result<()> {
        let generation = self
            .snapshot
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("party generation exhausted"))?;
        report.generation = generation;
        self.snapshot.timeline = Timeline {
            position: report
                .position
                .min(media.as_ref().map_or(0.0, |media| media.duration)),
            anchor_ms: now + START_LEAD_MS,
            paused: media.is_none(),
        };
        self.snapshot.media = media;
        self.snapshot.generation = generation;
        self.snapshot.error = None;
        self.intended_paused = false;
        self.held = false;
        // People already present chose to watch the next selection together:
        // hold its initial anchor while they resolve their own streams. New
        // arrivals during playback still join the barrier only once ready.
        // Transport sequence numbers and membership remain room-scoped.
        for member in self.members.values_mut() {
            member.report = LocalReport::default();
            let waiting_for_selection = self.snapshot.media.is_some()
                && !member.participant.is_host
                && now.saturating_sub(member.last_seen) <= DISCONNECTED_MS;
            member.admitted = member.participant.is_host || waiting_for_selection;
            member.stalled = waiting_for_selection;
            member.bad_since = None;
            member.recovered_since = None;
            if member.participant.is_host {
                member.report = report.clone();
                member.last_seen = now;
            }
        }
        self.tick(now);
        Ok(())
    }

    pub fn remove(&mut self, id: &str, now: u64) -> Result<()> {
        ensure!(id != self.snapshot.host_id, "host must end the party");
        ensure!(self.members.remove(id).is_some(), "party member not found");
        self.revoked.insert(id.into());
        self.tick(now);
        Ok(())
    }

    pub fn command(&mut self, command: HostCommand, now: u64) -> Result<()> {
        self.tick(now);
        ensure!(!self.snapshot.ended, "party has ended");
        let duration = self
            .snapshot
            .media
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("choose party media first"))?
            .duration;
        let current = self.snapshot.timeline.position_at(now as f64);
        match command {
            HostCommand::SetPaused(paused) => {
                self.intended_paused = paused;
                self.snapshot.timeline = Timeline {
                    position: current,
                    anchor_ms: now + if paused { 0 } else { START_LEAD_MS },
                    paused: paused || self.held,
                };
            }
            HostCommand::SeekAbsolute(position) | HostCommand::SeekRelative(position) => {
                ensure!(position.is_finite(), "invalid seek");
                let position = if matches!(command, HostCommand::SeekRelative(_)) {
                    current + position
                } else {
                    position
                };
                self.snapshot.timeline = Timeline {
                    position: position.clamp(0.0, duration),
                    anchor_ms: now + START_LEAD_MS,
                    paused: self.intended_paused || self.held,
                };
            }
        }
        self.tick(now);
        Ok(())
    }

    pub fn continue_without_waiting(&mut self, now: u64) {
        if self.snapshot.media.is_none() || self.snapshot.ended {
            self.tick(now);
            return;
        }
        for member in self.members.values_mut() {
            if !member.participant.is_host
                && (!member.participant.ready || !member.participant.connected)
            {
                member.admitted = false;
            }
        }
        self.intended_paused = false;
        self.snapshot.timeline = Timeline {
            position: self.snapshot.timeline.position_at(now as f64),
            anchor_ms: now + START_LEAD_MS,
            paused: self.held,
        };
        self.tick(now);
    }

    pub fn tick(&mut self, now: u64) {
        if now.saturating_sub(self.created) >= ROOM_LIFETIME_MS {
            self.snapshot.ended = true;
            self.snapshot.error = Some("party expired".into());
        }
        let mut waiting = Vec::new();
        for member in self.members.values_mut() {
            let age = now.saturating_sub(member.last_seen);
            // A slightly shorter compatible edition can reach EOF first. It
            // remains a visible, paused member without holding everyone else.
            if member.report.ended
                && !member.participant.is_host
                && member.report.generation == self.snapshot.generation
                && self.snapshot.media.as_ref().is_some_and(|media| {
                    media_matches(&media.identity, member.report.media.as_ref())
                })
            {
                member.admitted = false;
            }
            member.participant.connected = age <= DISCONNECTED_MS;
            let mut issue = self.snapshot.media.as_ref().map_or_else(
                || Some("waiting for host to choose media".into()),
                |media| reason(media, self.snapshot.generation, &member.report),
            );
            if !member.participant.connected {
                issue = Some("disconnected".into());
            }
            member.participant.ready = issue.is_none();
            member.participant.buffering = member.report.buffering;
            member.participant.position = member.report.position;
            member.participant.buffered_ahead = member.report.buffered_ahead;
            member.participant.reason = issue.clone();
            if issue.is_some() {
                member.recovered_since = None;
                let since = *member.bad_since.get_or_insert(now);
                if now.saturating_sub(since) >= STALL_DEBOUNCE_MS {
                    member.stalled = true;
                }
            } else {
                member.bad_since = None;
                let since = *member.recovered_since.get_or_insert(now);
                if !member.admitted || now.saturating_sub(since) >= RECOVER_MS {
                    member.admitted = true;
                    member.stalled = false;
                }
            }
            if !member.participant.is_host && age > DISCONNECT_GRACE_MS {
                member.admitted = false;
            }
            if member.participant.is_host && age > HOST_TIMEOUT_MS {
                self.snapshot.ended = true;
                self.snapshot.error = Some("host disconnected".into());
            }
            if self.snapshot.media.is_some()
                && member.admitted
                && member.stalled
                && (member.participant.is_host || self.snapshot.wait_for_guests)
            {
                waiting.push(member.participant.name.clone());
            }
        }
        let hold = !waiting.is_empty();
        if hold != self.held {
            let position = self.snapshot.timeline.position_at(now as f64);
            self.snapshot.timeline = Timeline {
                position,
                anchor_ms: now + START_LEAD_MS,
                paused: hold || self.intended_paused,
            };
            self.held = hold;
        }
        if self.snapshot.ended || self.snapshot.media.is_none() {
            self.snapshot.timeline.paused = true;
        }
        self.snapshot.waiting = waiting;
        self.snapshot.participants = self
            .members
            .values()
            .map(|m| m.participant.clone())
            .collect();
        self.snapshot.revision = self.snapshot.revision.saturating_add(1);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn media() -> PartyMedia {
        PartyMedia {
            identity: MediaIdentity {
                media_type: "movie".into(),
                title_id: "tt123".into(),
                title: "Example".into(),
                ..MediaIdentity::default()
            },
            season: None,
            episode: None,
            duration: 120.0,
        }
    }

    pub(crate) fn report() -> LocalReport {
        LocalReport {
            generation: 1,
            media: Some(media().identity),
            loaded: true,
            seekable: true,
            position: 10.0,
            duration: 120.0,
            buffered_ahead: 20.0,
            ready: true,
            ..LocalReport::default()
        }
    }

    pub(crate) fn room() -> HostRoom {
        HostRoom::new(
            "a".repeat(32),
            "host".into(),
            [7; 32],
            media(),
            report(),
            "Host".into(),
            0,
        )
    }

    #[test]
    fn timeline_is_host_led_scheduled_and_relative_seeks_use_anchor() {
        let mut room = room();
        assert_eq!(room.snapshot.timeline.position_at(500.0), 10.0);
        assert_eq!(room.snapshot.timeline.position_at(2_000.0), 11.0);
        room.command(HostCommand::SeekRelative(20.0), 2_000)
            .unwrap();
        assert_eq!(room.snapshot.timeline.position, 31.0);
        assert_eq!(room.snapshot.timeline.anchor_ms, 3_000);
        room.command(HostCommand::SetPaused(true), 4_000).unwrap();
        assert_eq!(room.snapshot.timeline.position_at(9_000.0), 32.0);
        assert!(
            room.command(HostCommand::SeekAbsolute(f64::NAN), 4_000)
                .is_err()
        );
    }

    #[test]
    fn capability_expiry_membership_and_queued_revocation() {
        let mut room = room();
        assert!(
            room.authorize(&"a".repeat(32), &[8; 32], "guest", 1)
                .is_err()
        );
        assert!(
            room.authorize(&"b".repeat(32), &[7; 32], "guest", 1)
                .is_err()
        );
        room.authorize(&"a".repeat(32), &[7; 32], "guest", 1)
            .unwrap();
        room.join("guest", "Guest".into(), 1, 1).unwrap();
        room.remove("guest", 2).unwrap();
        assert!(
            room.authorize(&"a".repeat(32), &[7; 32], "guest", 3)
                .is_err()
        );
        assert!(
            room.join("new", "New".into(), 1, INVITE_LIFETIME_MS + 1)
                .is_err()
        );
        assert!(
            room.authorize(&"a".repeat(32), &[7; 32], "new", ROOM_LIFETIME_MS)
                .is_err()
        );
    }

    #[test]
    fn unready_guests_do_not_stall_and_wrong_edition_never_admits() {
        let mut room = room();
        room.join("guest", "Guest".into(), 1, 100).unwrap();
        room.local_report(report(), 1_000).unwrap();
        assert!(room.snapshot.waiting.is_empty());
        let mut wrong = report();
        wrong.duration = 130.0;
        room.report("guest", wrong, 2, 1_100).unwrap();
        assert!(
            !room
                .snapshot
                .participants
                .iter()
                .find(|p| p.id == "guest")
                .unwrap()
                .ready
        );
        assert!(!room.members["guest"].admitted);
        let mut wrong = report();
        wrong.media.as_mut().unwrap().title_id = "different".into();
        room.report("guest", wrong, 3, 1_200).unwrap();
        assert!(!room.members["guest"].admitted);
        room.report("guest", report(), 4, 1_300).unwrap();
        assert!(room.members["guest"].admitted);
    }

    #[test]
    fn buffering_debounce_recovery_and_disconnect_grace_are_finite() {
        let mut room = room();
        room.join("guest", "Guest".into(), 1, 100).unwrap();
        room.report("guest", report(), 2, 200).unwrap();
        let mut stalled = report();
        stalled.buffering = true;
        room.report("guest", stalled.clone(), 3, 300).unwrap();
        assert!(!room.snapshot.timeline.paused);
        room.report("guest", stalled, 4, 1_100).unwrap();
        assert!(room.snapshot.timeline.paused);
        room.command(HostCommand::SetPaused(false), 1_100).unwrap();
        assert!(
            room.snapshot.timeline.paused,
            "manual resume cannot bypass wait policy"
        );
        room.report("guest", report(), 5, 1_200).unwrap();
        assert!(room.snapshot.timeline.paused);
        room.report("guest", report(), 6, 1_700).unwrap();
        assert!(!room.snapshot.timeline.paused);
        assert_eq!(room.snapshot.timeline.anchor_ms, 2_700);
        room.local_report(report(), 4_000).unwrap();
        room.local_report(report(), 4_900).unwrap();
        assert!(room.snapshot.timeline.paused);
        room.local_report(report(), 12_000).unwrap();
        assert!(
            !room.snapshot.timeline.paused,
            "disconnected guests cannot hold forever"
        );
        assert!(!room.members["guest"].admitted);
    }

    #[test]
    fn stale_generations_duplicates_and_left_members_cannot_mutate() {
        let mut room = room();
        room.join("guest", "Guest".into(), 1, 100).unwrap();
        room.report("guest", report(), 2, 200).unwrap();
        let mut stale = report();
        stale.generation = 0;
        stale.position = 99.0;
        room.report("guest", stale, 3, 300).unwrap();
        assert_eq!(room.members["guest"].report.position, 10.0);
        let mut duplicated = report();
        duplicated.position = 77.0;
        room.report("guest", duplicated, 2, 400).unwrap();
        assert_eq!(room.members["guest"].report.position, 10.0);
        room.remove("guest", 500).unwrap();
        assert!(room.report("guest", report(), 9, 600).is_err());
    }

    #[test]
    fn member_limit_and_unselected_host_playback_do_not_replace_shared_media() {
        let mut room = room();
        for n in 1..MAX_MEMBERS {
            room.join(&format!("g{n}"), "Guest".into(), 1, n as u64)
                .unwrap();
        }
        assert!(room.join("extra", "Extra".into(), 1, 50).is_err());
        let mut changed = report();
        changed.media.as_mut().unwrap().episode_id = "next".into();
        room.local_report(changed, 100).unwrap();
        assert!(!room.snapshot.ended);
        assert_eq!(room.snapshot.media, Some(media()));
        assert!(!room.members["host"].participant.ready);
    }

    #[test]
    fn host_cannot_resume_an_expired_foreground_session() {
        let mut room = room();
        room.local_report(report(), HOST_TIMEOUT_MS + 1).unwrap();
        assert!(room.snapshot.ended && room.snapshot.timeline.paused);
        assert_eq!(room.snapshot.error.as_deref(), Some("host disconnected"));
        assert!(
            room.command(HostCommand::SetPaused(false), HOST_TIMEOUT_MS + 2)
                .is_err()
        );
    }

    #[test]
    fn guest_eof_does_not_hold_room_and_host_eof_returns_to_lobby() {
        let mut room = room();
        room.join("guest", "Guest".into(), 1, 100).unwrap();
        room.report("guest", report(), 2, 200).unwrap();
        let eof = LocalReport {
            ended: true,
            position: 120.0,
            buffered_ahead: 0.0,
            ..report()
        };
        room.report("guest", eof.clone(), 3, 300).unwrap();
        room.local_report(report(), 1_200).unwrap();
        assert!(room.snapshot.waiting.is_empty());
        assert!(!room.snapshot.ended);
        assert!(!room.members["guest"].admitted);
        assert!(!room.members["guest"].participant.ready);
        let correction = drift_correction(&room.snapshot, &eof, 1_200);
        assert!(correction.paused && correction.seek_to.is_none());
        room.report("guest", report(), 4, 1_300).unwrap();
        assert!(room.members["guest"].admitted);
        room.local_report(eof, 1_400).unwrap();
        assert!(!room.snapshot.ended && room.snapshot.timeline.paused);
        assert!(room.snapshot.media.is_none());
        assert_eq!(room.snapshot.timeline.position, 0.0);
        assert_eq!(room.snapshot.generation, 2);
        assert_eq!(room.members.len(), 2);
        assert!(room.snapshot.error.is_none());
        // A final duplicate decoder event cannot end the lobby or next episode.
        room.local_report(
            LocalReport {
                ended: true,
                ..report()
            },
            1_500,
        )
        .unwrap();
        assert!(!room.snapshot.ended && room.snapshot.media.is_none());
        room.select_media(media(), report(), 1_600).unwrap();
        assert_eq!(room.snapshot.generation, 3);
        assert!(!room.snapshot.ended);
    }

    #[test]
    fn lobby_join_and_media_replacement_keep_room_membership_and_expiry() {
        let mut room = HostRoom::lobby("a".repeat(32), "host".into(), [7; 32], "Host".into(), 0);
        room.join("guest", "Guest".into(), 1, 100).unwrap();
        room.local_report(LocalReport::default(), 1_000).unwrap();
        assert!(room.snapshot.media.is_none());
        assert!(room.snapshot.waiting.is_empty());
        assert!(room.snapshot.timeline.paused && !room.snapshot.ended);
        assert!(
            room.command(HostCommand::SeekAbsolute(10.0), 1_000)
                .is_err()
        );
        let correction = drift_correction(&room.snapshot, &report(), 1_000);
        assert!(correction.paused && correction.seek_to.is_none() && correction.rate == 1.0);
        room.select_media(media(), report(), 1_100).unwrap();
        assert_eq!(room.snapshot.generation, 2);
        assert!(room.members["host"].participant.ready);
        let mut ready = report();
        ready.generation = 2;
        room.report("guest", ready, 2, 1_200).unwrap();
        assert!(room.members["guest"].admitted);
        room.clear_media(1_300).unwrap();
        assert_eq!(room.snapshot.generation, 3);
        assert!(room.snapshot.media.is_none() && room.snapshot.waiting.is_empty());
        assert!(!room.members["guest"].admitted);
        let mut next = media();
        next.identity.episode_id = "episode-2".into();
        let next_report = LocalReport {
            media: Some(next.identity.clone()),
            position: 0.0,
            ..report()
        };
        room.select_media(next.clone(), next_report, 1_400).unwrap();
        assert_eq!(room.snapshot.media, Some(next));
        assert_eq!(room.snapshot.generation, 4);
        assert_eq!(room.snapshot.room_id, "a".repeat(32));
        assert_eq!(room.capability, [7; 32]);
        assert_eq!(room.created, 0);
        assert_eq!(room.members.len(), 2);
        assert_eq!(room.members["guest"].last_sequence, 2);
        assert!(room.members["guest"].admitted);
        assert_eq!(room.snapshot.waiting, ["Guest"]);
        assert!(room.snapshot.timeline.paused);
        assert!(!room.members["guest"].participant.ready);
        let stale = LocalReport {
            ended: true,
            position: 120.0,
            ..report()
        };
        room.report("guest", stale.clone(), 3, 1_500).unwrap();
        room.local_report(stale, 1_500).unwrap();
        assert!(!room.snapshot.ended);
        assert_eq!(room.snapshot.timeline.position, 0.0);
        assert!(room.members["host"].participant.ready);
        assert!(!room.members["guest"].participant.ready);
        assert!(
            room.join("late", "Late".into(), 1, INVITE_LIFETIME_MS + 1)
                .is_err()
        );
        assert!(room.clear_media(ROOM_LIFETIME_MS).is_err());
        assert!(room.snapshot.ended);
    }

    #[test]
    fn lobby_members_hold_the_initial_anchor_and_replacements_until_ready() {
        let mut room = HostRoom::lobby("a".repeat(32), "host".into(), [7; 32], "Host".into(), 0);
        room.join("guest", "Guest".into(), 1, 100).unwrap();
        room.select_media(media(), report(), 200).unwrap();
        assert!(room.snapshot.timeline.paused);
        assert_eq!(room.snapshot.timeline.position_at(5_000.0), 10.0);
        assert_eq!(room.snapshot.waiting, ["Guest"]);
        let ready = LocalReport {
            generation: room.snapshot.generation,
            ..report()
        };
        room.report("guest", ready.clone(), 2, 300).unwrap();
        assert!(
            room.snapshot.timeline.paused,
            "readiness must settle before resuming"
        );
        room.report("guest", ready.clone(), 3, 800).unwrap();
        assert!(!room.snapshot.timeline.paused);
        assert_eq!(room.snapshot.timeline.anchor_ms, 1_800);
        room.select_media(media(), report(), 900).unwrap();
        assert_eq!(room.snapshot.waiting, ["Guest"]);
        assert!(room.snapshot.timeline.paused);
        // Even the same episode must report readiness for its new generation.
        room.report("guest", ready, 4, 1_000).unwrap();
        assert!(room.snapshot.timeline.paused);
        // The old decoder may still expose EOF while the app is waiting for
        // this generation's matching source; that is not this episode's EOF.
        room.report(
            "guest",
            LocalReport {
                generation: room.snapshot.generation,
                ended: true,
                ..LocalReport::default()
            },
            5,
            1_100,
        )
        .unwrap();
        assert!(room.snapshot.timeline.paused);
        let ready = LocalReport {
            generation: room.snapshot.generation,
            ..report()
        };
        room.report("guest", ready.clone(), 6, 1_200).unwrap();
        room.report("guest", ready, 7, 1_700).unwrap();
        assert!(!room.snapshot.timeline.paused);
        // A new arrival must choose a source before it can stall this playback.
        room.join("late", "Late".into(), 1, 1_800).unwrap();
        room.local_report(
            LocalReport {
                generation: room.snapshot.generation,
                ..report()
            },
            2_600,
        )
        .unwrap();
        assert!(!room.members["late"].admitted);
        assert!(room.snapshot.waiting.is_empty());
        assert!(!room.snapshot.timeline.paused);
    }

    #[test]
    fn initial_selection_wait_can_be_disabled_or_skipped_by_host() {
        for disable_wait in [false, true] {
            let mut room = room();
            room.join("guest", "Guest".into(), 1, 100).unwrap();
            room.select_media(media(), report(), 200).unwrap();
            assert!(room.snapshot.timeline.paused);
            if disable_wait {
                room.snapshot.wait_for_guests = false;
                room.tick(300);
            } else {
                room.continue_without_waiting(300);
                assert!(!room.members["guest"].admitted);
            }
            assert!(room.snapshot.waiting.is_empty());
            assert!(!room.snapshot.timeline.paused);
            assert_eq!(room.snapshot.timeline.anchor_ms, 1_300);
            room.clear_media(400).unwrap();
            assert!(room.snapshot.waiting.is_empty());
            assert!(room.snapshot.timeline.paused);
        }
        let mut room = room();
        room.join("guest", "Guest".into(), 1, 100).unwrap();
        room.snapshot.wait_for_guests = false;
        room.select_media(media(), report(), 200).unwrap();
        assert!(room.snapshot.waiting.is_empty());
        assert!(!room.snapshot.timeline.paused);
    }

    #[test]
    fn initial_selection_wait_has_finite_disconnect_grace() {
        let mut room = room();
        room.join("guest", "Guest".into(), 1, 100).unwrap();
        room.select_media(media(), report(), 200).unwrap();
        let current = LocalReport {
            generation: room.snapshot.generation,
            ..report()
        };
        room.local_report(current.clone(), 3_000).unwrap();
        assert!(!room.members["guest"].participant.connected);
        assert!(room.snapshot.timeline.paused);
        assert_eq!(room.snapshot.waiting, ["Guest"]);
        room.local_report(current, 10_101).unwrap();
        assert!(!room.members["guest"].admitted);
        assert!(room.snapshot.waiting.is_empty());
        assert!(!room.snapshot.timeline.paused);
        assert!(!room.snapshot.ended);
        // A member already disconnected when selection changes is not waited on.
        room.select_media(media(), report(), 10_200).unwrap();
        assert!(!room.members["guest"].admitted);
        assert!(room.snapshot.waiting.is_empty());
        assert!(!room.snapshot.timeline.paused);
    }

    #[test]
    fn clearing_media_preserves_revocation_and_does_not_revive_ended_rooms() {
        let mut room = room();
        room.join("guest", "Guest".into(), 1, 100).unwrap();
        room.remove("guest", 200).unwrap();
        room.clear_media(300).unwrap();
        room.select_media(media(), report(), 400).unwrap();
        assert!(
            room.authorize(&"a".repeat(32), &[7; 32], "guest", 500)
                .is_err()
        );
        assert!(
            room.select_media(media(), report(), HOST_TIMEOUT_MS + 401)
                .is_err()
        );
        assert!(room.snapshot.ended);
    }

    #[test]
    fn paused_alignment_clock_offset_and_drift_never_change_preferences() {
        let mut snapshot = room().snapshot;
        snapshot.clock_offset_ms = 5_000.0;
        snapshot.timeline = Timeline {
            position: 40.0,
            anchor_ms: 6_000,
            paused: false,
        };
        let local = report();
        let correction = drift_correction(&snapshot, &local, 500);
        assert!(correction.paused);
        assert_eq!(correction.seek_to, Some(40.0));
        let near = LocalReport {
            position: 39.3,
            ..report()
        };
        assert_eq!(drift_correction(&snapshot, &near, 500).seek_to, Some(40.0));
        let empty = LocalReport {
            position: 39.3,
            buffered_ahead: 0.0,
            ..report()
        };
        let correction = drift_correction(&snapshot, &empty, 2_000);
        assert!(correction.paused);
        assert_eq!(correction.seek_to, Some(41.0));
        let local = LocalReport {
            position: 40.4,
            ..report()
        };
        let correction = drift_correction(&snapshot, &local, 2_000);
        assert!(!correction.paused);
        assert_eq!(correction.rate, 1.03);
        assert_eq!(correction.seek_to, None);
        let local = LocalReport {
            position: 41.1,
            ..report()
        };
        assert_eq!(drift_correction(&snapshot, &local, 2_000).rate, 1.0);
        snapshot.connected = false;
        assert!(drift_correction(&snapshot, &local, 2_000).paused);
    }
}

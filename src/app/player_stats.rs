//! Read-only, source-owned diagnostics. One process-wide sample/copy permit
//! prevents overlapping workers across close/reopen, profile switches and Android
//! activity replacement. Only the UI thread touches Slint; workers return a whitelist.
use super::*;
use nova_player::DiagnosticsSnapshot;
use slint::ModelRc;
use std::time::Instant;

static SAMPLING: AtomicBool = AtomicBool::new(false);
static COPYING: AtomicBool = AtomicBool::new(false);
const INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Owner {
    profile: String,
    profile_epoch: u64,
    source: u64,
    torrent: Option<String>,
}

#[derive(Clone)]
struct Ticket {
    owner: Owner,
    revision: u64,
    request: u64,
    viewport: (u32, u32),
}

#[derive(Default)]
struct State {
    owner: Option<Owner>,
    visible: bool,
    revision: u64,
    next_request: u64,
    in_flight: Option<u64>,
    last_started: Option<Instant>,
    report: Option<Report>,
}

impl State {
    fn view(&mut self, owner: Option<Owner>, requested: bool) -> bool {
        let replaced = self.owner.is_some() && self.owner != owner;
        let visible = requested && owner.is_some() && !(self.visible && replaced);
        if self.owner == owner && self.visible == visible {
            return false;
        }
        self.revision = self.revision.wrapping_add(1);
        self.owner = owner;
        self.visible = visible;
        self.last_started = None;
        self.report = None;
        // An old worker keeps its slot until completion; invalidating its UI
        // owner must not allow another native query to overlap it.
        true
    }

    fn due(&self, now: Instant) -> bool {
        self.visible
            && self.owner.is_some()
            && self.in_flight.is_none()
            && self
                .last_started
                .is_none_or(|at| now.saturating_duration_since(at) >= INTERVAL)
    }

    fn begin(&mut self, now: Instant, viewport: (u32, u32)) -> Option<Ticket> {
        if !self.due(now) {
            return None;
        }
        let request = self.next_request.checked_add(1)?;
        self.next_request = request;
        self.in_flight = Some(request);
        self.last_started = Some(now);
        Some(Ticket {
            owner: self.owner.clone()?,
            revision: self.revision,
            request,
            viewport,
        })
    }

    fn accepts(&self, ticket: &Ticket, live: Option<&Owner>) -> bool {
        self.visible
            && self.revision == ticket.revision
            && self.owner.as_ref() == Some(&ticket.owner)
            && live == Some(&ticket.owner)
    }
}

#[derive(Default)]
pub(super) struct Controller(Mutex<State>);

struct Permit(&'static AtomicBool);
impl Permit {
    fn take(flag: &'static AtomicBool) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(flag))
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct SampleLease {
    controller: Arc<Controller>,
    request: u64,
    _permit: Permit,
}
impl Drop for SampleLease {
    fn drop(&mut self) {
        let mut state = self.controller.0.lock().unwrap();
        if state.in_flight == Some(self.request) {
            state.in_flight = None;
        }
    }
}

#[derive(Clone)]
struct Row {
    label: &'static str,
    value: String,
    section: bool,
}
#[derive(Clone)]
struct Report {
    rows: Vec<Row>,
}

fn number(value: f64, precision: usize) -> String {
    text::stats_number(value, precision)
}
fn known(value: Option<f64>, precision: usize, unit: &str, signed: bool) -> Option<String> {
    value
        .filter(|value| value.is_finite() && (signed || *value >= 0.0))
        .map(|value| format!("{}{unit}", number(value, precision)))
}
fn technical(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    // Native properties are technical identifiers, never a fallback title,
    // filename or source. Reject path/URL/header syntax rather than redact it.
    (!value.is_empty()
        && value.len() <= 96
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " ._-+(),".contains(c)))
    .then(|| value.to_string())
}
fn bytes(value: u64) -> String {
    let (divisor, unit) = if value >= 1 << 30 {
        (1u64 << 30, "GiB")
    } else if value >= 1 << 20 {
        (1u64 << 20, "MiB")
    } else if value >= 1 << 10 {
        (1u64 << 10, "KiB")
    } else {
        (1, "B")
    };
    format!(
        "{} {unit}",
        number(value as f64 / divisor as f64, usize::from(divisor > 1))
    )
}
fn join(values: impl IntoIterator<Item = Option<String>>) -> String {
    let value = values.into_iter().flatten().collect::<Vec<_>>().join(" · ");
    if value.is_empty() {
        "—".into()
    } else {
        value
    }
}
fn dimensions(width: Option<u64>, height: Option<u64>) -> Option<String> {
    width
        .zip(height)
        .filter(|(w, h)| *w > 0 && *h > 0)
        .map(|(w, h)| format!("{w} × {h}"))
}

impl Report {
    fn build(
        native: &DiagnosticsSnapshot,
        torrent: Option<nova_torrent::TorrentStats>,
        is_torrent: bool,
        viewport: (u32, u32),
    ) -> Self {
        // The engine also returns Default for an absent handle. Zero transfer
        // counters are real only once another observed fact proves this entry.
        let torrent = torrent.filter(|stats| {
            stats.total_bytes > 0
                || stats.ram_limit_bytes > 0
                || stats.downloaded_bytes > 0
                || stats.received_bytes > 0
                || stats.down_bps > 0
                || stats.peers > 0
                || stats.ram_used_bytes > 0
                || stats.buffer_target_bytes > 0
                || stats.progress > 0.0
        });
        let source = if is_torrent {
            if torrent.is_some_and(|stats| stats.ram_limit_bytes > 0) {
                text::tr("Torrent (RAM-only)")
            } else {
                text::tr("Torrent")
            }
        } else {
            match native.via_network {
                Some(true) => text::tr("Network"),
                Some(false) => text::tr("Local media"),
                None => "—",
            }
        };
        let viewport = dimensions(native.viewport_width, native.viewport_height)
            .or_else(|| dimensions(Some(u64::from(viewport.0)), Some(u64::from(viewport.1))));
        let codec = technical(native.video_codec.as_deref());
        let profile = technical(native.video_profile.as_deref());
        let codec = match (codec, profile) {
            (Some(codec), Some(profile)) => Some(format!("{codec} ({profile})")),
            (codec, _) => codec,
        };
        let decoder = technical(native.hwdec.as_deref()).map(|value| {
            if value == "no" {
                text::tr("Software").into()
            } else {
                value
            }
        });
        let mut report = Self { rows: Vec::new() };
        let mut row = |label, value: Option<String>, section| {
            report.rows.push(Row {
                label,
                value: value.unwrap_or_else(|| "—".into()),
                section,
            })
        };
        row("Source", Some(source.into()), false);
        row(
            "Video",
            Some(join([
                dimensions(native.video_width, native.video_height),
                known(native.fps, 3, " fps", false),
            ])),
            true,
        );
        row("Viewport", viewport, false);
        row(
            "Display refresh",
            known(native.display_fps, 3, " Hz", false),
            false,
        );
        row("Codec", codec, false);
        row("Decoder", decoder, false);
        row(
            "Video bitrate",
            known(
                native.video_bitrate_bps.map(|v| v / 1_000_000.0),
                2,
                " Mbps",
                false,
            ),
            false,
        );
        row(
            "Output drops",
            native.output_drops.map(|v| v.to_string()),
            false,
        );
        row(
            "Decoder drops",
            native.decoder_drops.map(|v| v.to_string()),
            false,
        );
        row(
            "Packet cache",
            Some(join([
                known(native.buffer_seconds, 1, " s", false),
                native.buffer_bytes.map(bytes),
            ])),
            true,
        );
        row(
            "Download",
            torrent
                .map(|stats| format!("{}/s · {}", bytes(stats.down_bps), text::peers(stats.peers))),
            false,
        );
        row(
            "RAM cache",
            torrent
                .filter(|stats| stats.ram_limit_bytes > 0)
                .map(|stats| {
                    format!(
                        "{} / {}",
                        bytes(stats.ram_used_bytes),
                        bytes(stats.ram_limit_bytes)
                    )
                }),
            false,
        );
        row(
            "Buffer target",
            torrent
                .filter(|stats| stats.ram_limit_bytes > 0 && stats.buffer_target_bytes > 0)
                .map(|stats| bytes(stats.buffer_target_bytes)),
            false,
        );
        row(
            "File size",
            torrent
                .filter(|stats| stats.total_bytes > 0)
                .map(|stats| bytes(stats.total_bytes)),
            false,
        );
        row(
            "Audio",
            Some(join([
                technical(native.audio_codec.as_deref()),
                known(
                    native.audio_sample_rate.map(|v| v as f64 / 1000.0),
                    1,
                    " kHz",
                    false,
                ),
                technical(native.audio_channels.as_deref()),
            ])),
            true,
        );
        row(
            "A/V sync",
            known(native.av_sync_seconds.map(|v| v * 1000.0), 1, " ms", true),
            false,
        );
        row(
            "Playback speed",
            known(native.playback_speed, 3, "×", false),
            false,
        );
        row("Renderer", technical(native.renderer.as_deref()), false);
        report
    }

    fn copy_text(&self) -> String {
        let mut output = format!(
            "Nova {}\nPlatform: {}\n",
            crate::version::DISPLAY,
            std::env::consts::OS
        );
        for row in &self.rows {
            output.push_str(&format!("{}: {}\n", row.label, row.value));
        }
        output
    }
    fn publish(&self, app: &AppWindow) {
        app.set_stats_rows(ModelRc::new(VecModel::from(
            self.rows
                .iter()
                .map(|row| nova_ui::PlayerStatsRow {
                    label: text::stats_label(row.label).into(),
                    value: row.value.as_str().into(),
                    section_start: row.section,
                })
                .collect::<Vec<_>>(),
        )));
    }
}

impl Bridge {
    fn stats_owner(&self) -> Option<Owner> {
        let app = self.app()?;
        if !app.get_player_open()
            || app.get_profiles_require_selection()
            || profile_policy::entry_blocked()
        {
            return None;
        }
        let state = self.shared.lock().ok()?;
        if !state.remote_playback.can_play_locally()
            || state.casting.active
            || state.casting.loading
        {
            return None;
        }
        if state.party.active()
            && !state
                .playback_intent
                .as_ref()
                .is_some_and(|intent| intent.party_bound_to(&state.party))
        {
            return None;
        }
        let profile = profiles::capture();
        Some(Owner {
            profile: profile.id,
            profile_epoch: profile.epoch,
            source: self.player.screenshot_session(),
            torrent: state.active_torrent.clone(),
        })
    }

    pub(super) fn stats_visibility_changed(&self, visible: bool) {
        self.refresh_player_stats(visible);
    }

    pub(super) fn player_stats_tick(&self) {
        if let Some(app) = self.app() {
            self.refresh_player_stats(app.get_stats_visible());
        }
    }

    fn refresh_player_stats(&self, visible: bool) {
        let Some(app) = self.app() else {
            return;
        };
        let owner = self.stats_owner();
        let controller = self.player_stats.clone();
        let (changed, visible) = {
            let mut state = controller.0.lock().unwrap();
            let changed = state.view(owner, visible);
            (changed, state.visible)
        };
        if changed {
            app.set_stats_rows(ModelRc::new(VecModel::<nova_ui::PlayerStatsRow>::default()));
            app.set_stats_notice(if visible {
                text::tr("Waiting for playback data…").into()
            } else {
                "".into()
            });
        }
        if app.get_stats_visible() != visible {
            app.set_stats_visible(visible);
        }
        if !visible {
            return;
        }
        let now = Instant::now();
        if !controller.0.lock().unwrap().due(now) {
            return;
        }
        let Some(permit) = Permit::take(&SAMPLING) else {
            return;
        };
        let viewport = app.window().size();
        let Some(ticket) = controller
            .0
            .lock()
            .unwrap()
            .begin(now, (viewport.width, viewport.height))
        else {
            return;
        };
        let lease = SampleLease {
            controller: controller.clone(),
            request: ticket.request,
            _permit: permit,
        };
        let player = self.player.clone();
        let bridge = self.clone();
        let _ = thread::Builder::new()
            .name("nova-player-stats".into())
            .spawn(move || {
                let sample = player.diagnostics_snapshot();
                let torrent = ticket
                    .owner
                    .torrent
                    .as_ref()
                    .and_then(|hash| crate::torrent::engine().map(|engine| engine.stats(hash)));
                let _ = slint::invoke_from_event_loop(move || {
                    let _lease = lease;
                    if !bridge.app().is_some_and(|app| app.get_stats_visible()) {
                        return;
                    }
                    let live = bridge.stats_owner();
                    let (report, first) = {
                        let mut state = controller.0.lock().unwrap();
                        if !state.accepts(&ticket, live.as_ref()) {
                            return;
                        }
                        let Some(sample) =
                            sample.filter(|sample| sample.session == ticket.owner.source)
                        else {
                            return;
                        };
                        let report = Report::build(
                            &sample,
                            torrent,
                            ticket.owner.torrent.is_some(),
                            ticket.viewport,
                        );
                        let first = state.report.is_none();
                        state.report = Some(report.clone());
                        (report, first)
                    };
                    if let Some(app) = bridge.app() {
                        report.publish(&app);
                        // Periodic samples must not immediately erase copy feedback.
                        if first {
                            app.set_stats_notice("".into());
                        }
                    }
                });
            });
    }

    pub(super) fn copy_player_stats(&self) {
        let Some(app) = self.app() else {
            return;
        };
        let live = self.stats_owner();
        let controller = self.player_stats.clone();
        let (ticket, report) = {
            let state = controller.0.lock().unwrap();
            let Some(owner) = state
                .owner
                .clone()
                .filter(|owner| state.visible && live.as_ref() == Some(owner))
            else {
                return;
            };
            let Some(report) = state.report.clone() else {
                app.set_stats_notice(text::tr("Diagnostics are not available yet.").into());
                return;
            };
            (
                Ticket {
                    owner,
                    revision: state.revision,
                    request: 0,
                    viewport: (0, 0),
                },
                report,
            )
        };
        let Some(permit) = Permit::take(&COPYING) else {
            return;
        };
        let target = clipboard::prepare_copy();
        let bridge = self.clone();
        let _ = thread::Builder::new()
            .name("nova-stats-copy".into())
            .spawn(move || {
                let result = target.and_then(|target| target.copy(&report.copy_text()));
                let _ = slint::invoke_from_event_loop(move || {
                    let _permit = permit;
                    if !bridge.app().is_some_and(|app| app.get_stats_visible()) {
                        return;
                    }
                    let live = bridge.stats_owner();
                    if !controller.0.lock().unwrap().accepts(&ticket, live.as_ref()) {
                        return;
                    }
                    if let Some(app) = bridge.app() {
                        app.set_stats_notice(
                            text::tr(if result.is_ok() {
                                "Diagnostics copied."
                            } else {
                                "Could not copy diagnostics."
                            })
                            .into(),
                        );
                    }
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(source: u64) -> Owner {
        Owner {
            profile: "private-profile".into(),
            profile_epoch: 3,
            source,
            torrent: Some("private-infohash".into()),
        }
    }
    fn value<'a>(report: &'a Report, label: &str) -> &'a str {
        &report
            .rows
            .iter()
            .find(|row| row.label == label)
            .unwrap()
            .value
    }

    #[test]
    fn diagnostics_units_keep_packet_cache_torrent_ram_and_actual_speed_distinct() {
        text::with_language(nova_config::Language::English, || {
            let native = DiagnosticsSnapshot {
                video_width: Some(1920),
                video_height: Some(1080),
                fps: Some(23.976),
                video_codec: Some("h264".into()),
                video_profile: Some("High".into()),
                hwdec: Some("vaapi".into()),
                video_bitrate_bps: Some(5_250_000.0),
                output_drops: Some(4),
                decoder_drops: Some(0),
                buffer_seconds: Some(12.5),
                buffer_bytes: Some(64 << 20),
                audio_codec: Some("aac".into()),
                audio_sample_rate: Some(48_000),
                audio_channels: Some("stereo".into()),
                av_sync_seconds: Some(-0.003),
                playback_speed: Some(1.545),
                renderer: Some("gpu".into()),
                ..Default::default()
            };
            let torrent = nova_torrent::TorrentStats {
                down_bps: 1 << 20,
                peers: 3,
                ram_used_bytes: 128 << 20,
                ram_limit_bytes: 512 << 20,
                buffer_target_bytes: 384 << 20,
                total_bytes: 1 << 30,
                ..Default::default()
            };
            let report = Report::build(&native, Some(torrent), true, (1280, 720));
            assert_eq!(value(&report, "Source"), "Torrent (RAM-only)");
            assert_eq!(value(&report, "Video"), "1920 × 1080 · 23.976 fps");
            assert_eq!(value(&report, "Viewport"), "1280 × 720");
            assert_eq!(
                value(&report, "Display refresh"),
                "—",
                "No invented display refresh rate"
            );
            assert_eq!(value(&report, "Playback speed"), "1.545×");
            assert_eq!(
                value(&report, "Download"),
                "1.0 MiB/s · 3 peers",
                "Torrent down_bps is bytes/sec, not bits/sec"
            );
            assert_eq!(value(&report, "Packet cache"), "12.5 s · 64.0 MiB");
            assert_eq!(value(&report, "RAM cache"), "128.0 MiB / 512.0 MiB");
            assert_eq!(value(&report, "Buffer target"), "384.0 MiB");
            assert_eq!(value(&report, "Video bitrate"), "5.25 Mbps");
            assert_eq!(value(&report, "Audio"), "aac · 48.0 kHz · stereo");
            assert_eq!(value(&report, "A/V sync"), "-3.0 ms");
            assert_eq!(value(&report, "Output drops"), "4");
            assert_eq!(
                value(&report, "Decoder drops"),
                "0",
                "Counts have no fabricated presented-frame denominator"
            );
        });
    }

    #[test]
    fn unavailable_stats_stay_unknown_and_copy_only_the_technical_whitelist() {
        text::with_language(nova_config::Language::English, || {
            let native = DiagnosticsSnapshot {
                fps: Some(f64::NAN),
                playback_speed: Some(f64::INFINITY),
                video_codec: Some("https://private.example/video?token=secret".into()),
                video_profile: Some("Authorization: Bearer secret".into()),
                hwdec: Some("/home/alice/private.mkv".into()),
                audio_codec: Some("C:\\Users\\alice\\secret.mkv".into()),
                renderer: Some("gpu\naccount-name".into()),
                ..Default::default()
            };
            let report = Report::build(&native, None, true, (0, 0));
            assert_eq!(
                value(&report, "Source"),
                "Torrent",
                "RAM-only requires observed nonzero RAM capacity"
            );
            for label in [
                "Video",
                "Viewport",
                "Display refresh",
                "Codec",
                "Decoder",
                "Playback speed",
                "RAM cache",
                "Download",
                "Audio",
                "Renderer",
            ] {
                assert_eq!(value(&report, label), "—", "{label}");
            }
            let copied = report.copy_text();
            assert!(copied.starts_with(&format!(
                "Nova {}\nPlatform: {}\n",
                crate::version::DISPLAY,
                std::env::consts::OS
            )));
            for private in [
                "https://",
                "Authorization",
                "Bearer",
                "alice",
                "secret",
                "private-profile",
                "private-infohash",
                "account-name",
            ] {
                assert!(!copied.contains(private), "Copy must not include {private}");
            }
            let disk = Report::build(
                &DiagnosticsSnapshot::default(),
                Some(nova_torrent::TorrentStats::default()),
                true,
                (0, 0),
            );
            assert_eq!(value(&disk, "Source"), "Torrent");
            assert_eq!(value(&disk, "RAM cache"), "—");
            assert_eq!(
                value(&disk, "Download"),
                "—",
                "An absent handle's default counters are not measurements"
            );
            let idle = Report::build(
                &DiagnosticsSnapshot::default(),
                Some(nova_torrent::TorrentStats {
                    total_bytes: 1024,
                    ..Default::default()
                }),
                true,
                (0, 0),
            );
            assert_eq!(
                value(&idle, "Download"),
                "0 B/s · 0 peers",
                "Known file size makes idle zero counters meaningful"
            );
        });
        text::with_language(nova_config::Language::Croatian, || {
            let report = Report::build(
                &DiagnosticsSnapshot {
                    playback_speed: Some(1.545),
                    ..Default::default()
                },
                None,
                false,
                (0, 0),
            );
            assert_eq!(value(&report, "Playback speed"), "1,545×");
        });
    }

    #[test]
    fn sampler_is_visibility_paced_and_a_close_reopen_cannot_accept_the_old_delivery() {
        let now = Instant::now();
        let mut state = State::default();
        state.view(Some(owner(1)), false);
        assert!(!state.due(now));
        state.view(Some(owner(1)), true);
        let first = state.begin(now, (640, 360)).unwrap();
        assert!(
            !state.due(now + INTERVAL),
            "An in-flight native call occupies the only slot"
        );
        state.in_flight = None;
        assert!(!state.due(now + Duration::from_millis(999)));
        assert!(state.due(now + INTERVAL));
        state.view(Some(owner(1)), false);
        state.view(Some(owner(1)), true);
        assert!(
            !state.accepts(&first, Some(&owner(1))),
            "A late sample/copy result cannot enter a reopened panel"
        );
        let reopened = state.begin(now + INTERVAL, (640, 360)).unwrap();
        assert!(state.accepts(&reopened, Some(&owner(1))));
    }

    #[test]
    fn source_profile_torrent_and_ownership_changes_reject_late_samples_and_copy_notices() {
        for change in [
            "source",
            "profile",
            "profile-epoch",
            "torrent",
            "hidden",
            "closed",
        ] {
            let mut state = State::default();
            state.view(Some(owner(1)), true);
            let ticket = state.begin(Instant::now(), (0, 0)).unwrap();
            let mut current = owner(1);
            match change {
                "source" => current.source += 2,
                "profile" => current.profile = "another-profile".into(),
                "profile-epoch" => current.profile_epoch += 1,
                "torrent" => current.torrent = Some("another-infohash".into()),
                "hidden" => {
                    state.view(Some(current.clone()), false);
                }
                "closed" => {
                    state.view(None, false);
                }
                _ => unreachable!(),
            }
            assert!(
                !state.accepts(&ticket, Some(&current)),
                "{change}: live ownership wins even before the next timer tick"
            );
        }
    }

    #[test]
    fn an_invalidated_worker_keeps_its_slot_and_raii_releases_it_after_delivery_failure() {
        static GATE: AtomicBool = AtomicBool::new(false);
        let controller = Arc::new(Controller::default());
        let now = Instant::now();
        let ticket = {
            let mut state = controller.0.lock().unwrap();
            state.view(Some(owner(1)), true);
            state.begin(now, (0, 0)).unwrap()
        };
        let lease = SampleLease {
            controller: controller.clone(),
            request: ticket.request,
            _permit: Permit::take(&GATE).unwrap(),
        };
        {
            let mut state = controller.0.lock().unwrap();
            state.view(None, false);
            state.view(Some(owner(3)), true);
            assert!(
                !state.due(now + INTERVAL),
                "Closing/reopening or changing profiles cannot overlap a blocked worker"
            );
        }
        assert!(
            Permit::take(&GATE).is_none(),
            "The same process gate also excludes replacement activities"
        );
        drop(lease); // Also covers a dropped spawn/event-loop closure.
        assert!(!GATE.load(Ordering::Acquire));
        assert!(controller.0.lock().unwrap().due(now + INTERVAL));
        let replacement = controller
            .0
            .lock()
            .unwrap()
            .begin(now + INTERVAL, (0, 0))
            .unwrap();
        assert!(replacement.request > ticket.request);
        assert!(
            !controller
                .0
                .lock()
                .unwrap()
                .accepts(&ticket, Some(&owner(3)))
        );
    }
}

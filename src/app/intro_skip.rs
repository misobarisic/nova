//! Public timing lookups; missing/ambiguous identity and mismatched cuts stay inert.
use super::*;
use crate::download::{
    CancellationToken, DownloadIntroRequest, DownloadIntroSegment, DownloadIntroTimings,
};
use crate::providers::{self, ExternalId, IdNamespace, IdResolution};
use serde_json::Value;
use std::time::Duration;

static FETCHING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug)]
struct Segment {
    kind: i32, // 1 intro, 2 recap; credits and mixed-story openings are excluded.
    start: f64,
    end: f64,
    consumed: bool,
    confidence_percent: Option<u8>,
}

impl Segment {
    fn persisted(&self) -> DownloadIntroSegment {
        DownloadIntroSegment {
            kind: self.kind,
            start_ms: (self.start * 1000.0).round() as u64,
            end_ms: (self.end * 1000.0).round() as u64,
            prompt_only: false,
            confidence_percent: self.confidence_percent,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Session {
    native: u64,
    requested: bool,
    duration: f64,
    segments: Vec<Segment>,
    seek_floor: f64,
    observed: f64,
}

impl Session {
    fn active(&self, position: f64) -> Option<usize> {
        self.segments
            .iter()
            .position(|s| position.is_finite() && position >= s.start && position < s.end - 0.5)
    }

    fn offer(&mut self, position: f64, auto: bool, minimum: u8) -> Option<(i32, f64, bool)> {
        for segment in &mut self.segments {
            if position >= segment.end {
                segment.consumed = true;
            }
        }
        let index = self.active(position)?;
        let segment = &mut self.segments[index];
        let skip =
            auto && segment.confidence_percent.is_none_or(|c| c >= minimum) && !segment.consumed;
        if skip {
            segment.consumed = true;
        }
        Some((segment.kind, segment.end, skip))
    }

    fn publish(&mut self, native: u64, mut segments: Vec<Segment>) {
        if self.native != native {
            return;
        }
        for segment in &mut segments {
            segment.consumed = segment.start <= self.seek_floor && self.seek_floor > 0.0;
        }
        self.segments = segments;
    }

    fn seek(&mut self, from: f64, to: f64) {
        // Remember explicit seeks even while the timing request is pending.
        // Returning to a skipped segment always leaves manual control available.
        self.seek_floor = self.seek_floor.max(from).max(to).max(self.observed);
        // A seek before the first native sample has no reliable origin.
        // Prefer manual control for this session over surprising replay skips.
        if self.seek_floor == 0.0 {
            self.seek_floor = f64::MAX;
        }
        for segment in &mut self.segments {
            if segment.start <= self.seek_floor {
                segment.consumed = true;
            }
        }
    }
}

#[derive(Clone, Debug)]
enum Lookup {
    IntroDb {
        key: String,
        id: String,
        season: u32,
        episode: u32,
    },
    AniSkip {
        mal: u32,
        episode: u32,
    },
}

fn lookup(ids: &providers::ExternalIds, season: u32, episode: u32) -> Option<Lookup> {
    if season == 0 || episode == 0 {
        return None;
    }
    // MAL anime IDs name a specific release/cour, not a multi-season franchise.
    // Never reinterpret season two's local episode numbering as MAL episode two.
    if season == 1
        && let IdResolution::Unique(ExternalId::MalAnime(mal)) =
            ids.resolve_id(IdNamespace::MalAnime)
    {
        return Some(Lookup::AniSkip {
            mal: mal.get(),
            episode,
        });
    }
    if let IdResolution::Unique(ExternalId::TmdbTv(id)) = ids.resolve_id(IdNamespace::TmdbTv) {
        return Some(Lookup::IntroDb {
            key: "tmdb_id".into(),
            id: id.to_string(),
            season,
            episode,
        });
    }
    if let IdResolution::Unique(ExternalId::Imdb(id)) = ids.resolve_id(IdNamespace::Imdb) {
        return Some(Lookup::IntroDb {
            key: "imdb_id".into(),
            id,
            season,
            episode,
        });
    }
    None
}

impl Lookup {
    fn persisted(&self) -> DownloadIntroRequest {
        match self {
            Self::IntroDb {
                key,
                id,
                season,
                episode,
            } => DownloadIntroRequest::IntroDb {
                key: key.clone(),
                id: id.clone(),
                season: *season,
                episode: *episode,
            },
            Self::AniSkip { mal, episode } => DownloadIntroRequest::AniSkip {
                mal: *mal,
                episode: *episode,
            },
        }
    }

    fn restored(request: &DownloadIntroRequest) -> Option<Self> {
        match request {
            DownloadIntroRequest::IntroDb {
                key,
                id,
                season,
                episode,
            } => {
                // Local JSON is not trusted input for a public request URL.
                let valid = match key.as_str() {
                    "imdb_id" => id
                        .strip_prefix("tt")
                        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())),
                    "tmdb_id" => id.parse::<u32>().is_ok_and(|id| id > 0),
                    _ => false,
                };
                (valid && *season > 0 && *episode > 0).then(|| Self::IntroDb {
                    key: key.clone(),
                    id: id.clone(),
                    season: *season,
                    episode: *episode,
                })
            }
            DownloadIntroRequest::AniSkip { mal, episode } => {
                (*mal > 0 && *episode > 0).then_some(Self::AniSkip {
                    mal: *mal,
                    episode: *episode,
                })
            }
        }
    }

    fn url(&self, duration: f64) -> String {
        match self {
            Self::IntroDb {
                key,
                id,
                season,
                episode,
            } => {
                // IDs have already passed typed validation; encode independently
                // so future namespace formats cannot inject query parameters.
                let encoded: String = id
                    .bytes()
                    .map(|byte| {
                        if byte.is_ascii_alphanumeric() {
                            (byte as char).to_string()
                        } else {
                            format!("%{byte:02X}")
                        }
                    })
                    .collect();
                format!(
                    "https://api.theintrodb.org/v4/media?{key}={encoded}&season={season}&episode={episode}&duration_ms={:.0}&details=true",
                    duration * 1000.0
                )
            }
            Self::AniSkip { mal, episode } => format!(
                "https://api.aniskip.com/v2/skip-times/{mal}/{episode}?types%5B%5D=op&types%5B%5D=recap&episodeLength={duration:.3}"
            ),
        }
    }
}

/// Capture coordinates and verified provider IDs while the episode metadata is
/// available. The download manifest survives metadata-cache eviction.
pub(super) fn download_request(
    media_type: &str,
    media_id: &str,
    request_id: &str,
) -> Option<DownloadIntroRequest> {
    if media_type == "movie" {
        return None;
    }
    let episode = read_episodes_cache_for(media_type, media_id)?
        .into_iter()
        .find(|v| v.id == request_id)?;
    let ids = tracking::current_source_ids(media_type, media_id);
    lookup(&ids, episode.season?, episode.episode_number()?).map(|request| request.persisted())
}

/// This remains part of the transfer worker, so Android's download service
/// keeps it alive. No locks, native decoder, or playback connection are shared.
pub(super) fn prepare_download(
    request: &DownloadIntroRequest,
    path: &std::path::Path,
    cancel: &CancellationToken,
) -> Option<DownloadIntroTimings> {
    if cancel.is_cancelled() {
        return None;
    }
    let before = DownloadIntroTimings::capture(path, 0, Vec::new())?;
    let duration = crate::player::probe_local_duration(path)?;
    if !before.matches_path(path) || cancel.is_cancelled() {
        return None;
    }
    let lookup = Lookup::restored(request)?;
    let segments = fetch_download_segments(
        &lookup,
        duration,
        cancel,
        |url| net::fetch_metadata_bytes(url, Duration::from_secs(8), 128 * 1024).ok(),
        |delay| {
            // Short waits keep removal/pause responsive during retry backoff.
            for _ in 0..delay * 10 {
                if cancel.is_cancelled() {
                    return false;
                }
                thread::sleep(Duration::from_millis(100));
            }
            !cancel.is_cancelled()
        },
    )?;
    if !before.matches_path(path) || cancel.is_cancelled() {
        return None;
    }
    DownloadIntroTimings::capture(
        path,
        (duration * 1000.0).round() as u64,
        segments.iter().map(Segment::persisted).collect(),
    )
}

fn fetch_download_segments(
    lookup: &Lookup,
    duration: f64,
    cancel: &CancellationToken,
    mut fetch: impl FnMut(&str) -> Option<Vec<u8>>,
    mut backoff: impl FnMut(u64) -> bool,
) -> Option<Vec<Segment>> {
    let url = lookup.url(duration);
    for attempt in 0..3 {
        if cancel.is_cancelled() {
            return None;
        }
        if let Some(bytes) = fetch(&url) {
            // A successful empty response is authoritative: no timings for this
            // cut. Retry transport failures, not unsupported episodes.
            let segments = parse(lookup, &bytes, duration);
            return (!segments.is_empty()).then_some(segments);
        }
        if attempt < 2 && !backoff(if attempt == 0 { 1 } else { 3 }) {
            return None;
        }
    }
    None
}

fn restore_segments(timings: &DownloadIntroTimings, duration: f64) -> Vec<Segment> {
    if !compatible_duration(timings.duration_ms as f64 / 1000.0, duration)
        || timings.segments.len() > 32
    {
        return Vec::new();
    }
    let mut segments = Vec::new();
    for saved in &timings.segments {
        let start = saved.start_ms as f64 / 1000.0;
        let end = saved.end_ms as f64 / 1000.0;
        if !matches!(saved.kind, 1 | 2)
            || !valid_interval(start, end, duration)
            || saved.confidence_percent.is_some_and(|c| c > 100)
        {
            return Vec::new();
        }
        segments.push(Segment {
            kind: saved.kind,
            start,
            end,
            consumed: false,
            // The first prompt-only APK did not retain the exact score. Keep
            // those legacy entries manual unless the viewer chooses Any.
            confidence_percent: if saved.prompt_only {
                Some(0)
            } else {
                saved.confidence_percent
            },
        });
    }
    segments.sort_by(|a, b| a.start.total_cmp(&b.start));
    if segments.windows(2).any(|p| p[0].end > p[1].start) {
        return Vec::new();
    }
    segments
}

fn valid_interval(start: f64, end: f64, duration: f64) -> bool {
    start.is_finite()
        && end.is_finite()
        && start >= 0.0
        && end > start + 1.0
        && end - start <= 600.0
        && end < duration - 5.0
        && end <= duration * 0.6
}

fn compatible_duration(actual: f64, expected: f64) -> bool {
    actual.is_finite() && expected.is_finite() && actual > 0.0 && (actual - expected).abs() <= 5.0
}

fn parse(lookup: &Lookup, bytes: &[u8], duration: f64) -> Vec<Segment> {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return Vec::new();
    };
    let mut segments = Vec::new();
    let mut add = |kind, start: f64, end: f64, confidence_percent| {
        // Bounded, finite early-episode intervals with content left afterwards.
        if valid_interval(start, end, duration) {
            segments.push(Segment {
                kind,
                start,
                end,
                consumed: false,
                confidence_percent,
            });
        }
    };
    match lookup {
        Lookup::AniSkip { .. } => {
            if value["found"] != true {
                return segments;
            }
            for row in value["results"].as_array().into_iter().flatten().take(32) {
                let kind = match row["skipType"].as_str() {
                    Some("op") => 1,
                    Some("recap") => 2,
                    _ => continue,
                };
                if !row["episodeLength"]
                    .as_f64()
                    .is_some_and(|d| compatible_duration(d, duration))
                {
                    continue;
                }
                if let (Some(start), Some(end)) = (
                    row["interval"]["startTime"].as_f64(),
                    row["interval"]["endTime"].as_f64(),
                ) {
                    add(kind, start, end, None);
                }
            }
        }
        Lookup::IntroDb {
            season, episode, ..
        } => {
            if value["type"] != "tv"
                || value["season"].as_u64() != Some(*season as u64)
                || value["episode"].as_u64() != Some(*episode as u64)
                || !value["group_duration_ms"]
                    .as_f64()
                    .is_some_and(|d| compatible_duration(d / 1000.0, duration))
            {
                return segments;
            }
            for (field, kind) in [("intro", 1), ("recap", 2)] {
                for row in value[field].as_array().into_iter().flatten().take(16) {
                    let Some(confidence) = row["confidence"]
                        .as_f64()
                        .filter(|c| c.is_finite() && *c >= 0.0 && *c <= 1.0)
                    else {
                        continue;
                    };
                    let start = if row["start_ms"].is_null() {
                        Some(0.0)
                    } else {
                        row["start_ms"].as_f64()
                    };
                    if let (Some(start), Some(end)) = (start, row["end_ms"].as_f64()) {
                        // Keep evidence independent of the selected preset so
                        // changing it works immediately, including offline.
                        add(
                            kind,
                            start / 1000.0,
                            end / 1000.0,
                            Some((confidence * 100.0).floor() as u8),
                        );
                    }
                }
            }
        }
    }
    segments.sort_by(|a, b| a.start.total_cmp(&b.start));
    // Conflicting/overlapping crowd timestamps cannot choose a safe target.
    if segments.windows(2).any(|pair| pair[0].end > pair[1].start) {
        return Vec::new();
    }
    segments
}

impl Bridge {
    fn intro_skip_eligible(&self, app: &AppWindow) -> bool {
        remote_playback::is_foreground()
            && self.player.playback_readiness().seekable
            && app.get_player_open()
            && app.get_playback_started()
            && !app.get_resume_prompt_visible()
            && !app.get_party_page_open()
            && !app.get_remote_controls_open()
            && !app.get_remote_devices_open()
            && !self.party_active()
            && self
                .shared
                .lock()
                .unwrap()
                .remote_playback
                .can_play_locally()
    }

    pub(super) fn note_intro_seek(&self, from: f64, to: f64) {
        let native = self.player.screenshot_session();
        if let Some(target) = self.shared.lock().unwrap().playback.as_mut() {
            if target.intro_skip.native != native {
                target.intro_skip = Session {
                    native,
                    ..Session::default()
                };
            }
            target.intro_skip.seek(from.max(target.last_pos), to);
        }
    }

    pub(super) fn refresh_intro_skip(&self) {
        let Some(app) = self.app() else { return };
        app.set_skip_segment_kind(self.intro_skip_kind(&app));
    }

    fn intro_skip_kind(&self, app: &AppWindow) -> i32 {
        let (global, minimum) = {
            let state = self.shared.lock().unwrap();
            (
                state.cache_settings.intro_skip_mode,
                state.cache_settings.intro_skip_confidence.percent(),
            )
        };
        let mode = self.player.intro_skip_mode(global);
        if mode == nova_config::IntroSkipMode::Off || !self.intro_skip_eligible(app) {
            return 0;
        }
        let duration = app.get_duration() as f64;
        if !duration.is_finite() || duration <= 10.0 {
            return 0;
        }
        let native = self.player.screenshot_session();
        let download = self
            .shared
            .lock()
            .unwrap()
            .playback
            .as_ref()
            .filter(|p| p.intro_skip.native != native || !p.intro_skip.requested)
            .and_then(|p| p.intro_download.clone());
        let cached = download
            .as_ref()
            .and_then(|job| self.downloads.cached_intro(job));
        let request = {
            let mut state = self.shared.lock().unwrap();
            let Some(target) = state.playback.as_mut().filter(|p| p.session != 0) else {
                return 0;
            };
            if target.intro_skip.native != native {
                target.intro_skip = Session {
                    native,
                    ..Session::default()
                };
            }
            target.intro_skip.observed = target.intro_skip.observed.max(app.get_position() as f64);
            if target.intro_skip.requested {
                None
            } else {
                if cached.is_none() && FETCHING.load(Ordering::Acquire) {
                    return 0;
                }
                target.intro_skip.requested = true;
                target.intro_skip.duration = duration;
                let session = target.session;
                if let Some(timings) = cached {
                    target
                        .intro_skip
                        .publish(native, restore_segments(&timings, duration));
                    None
                } else {
                    let persisted_lookup = download
                        .as_ref()
                        .and_then(|job| job.intro_request.as_ref())
                        .and_then(Lookup::restored);
                    let request = persisted_lookup.or_else(|| {
                        target.history.as_ref().and_then(|h| {
                            let ids = tracking::current_source_ids(&h.type_, &target.series_id);
                            lookup(&ids, h.season?, h.episode?)
                        })
                    });
                    request.map(|lookup| (session, lookup, download.clone()))
                }
            }
        };
        if let Some((session, lookup, download)) = request {
            // This path runs on the UI thread: at most one public timing GET
            // runs at a time even during rapid episode/source replacements.
            FETCHING.store(true, Ordering::Release);
            let bridge = self.clone();
            thread::spawn(move || {
                let stamp = download
                    .as_ref()
                    .and_then(|job| job.artifact_path.as_deref())
                    .and_then(|path| DownloadIntroTimings::capture(path, 0, Vec::new()));
                let segments = net::fetch_metadata_bytes(
                    &lookup.url(duration),
                    Duration::from_secs(8),
                    128 * 1024,
                )
                .map(|bytes| parse(&lookup, &bytes, duration))
                .unwrap_or_default();
                if !segments.is_empty()
                    && let (Some(job), Some(mut stamp)) = (download.as_ref(), stamp)
                {
                    stamp.duration_ms = (duration * 1000.0).round() as u64;
                    stamp.segments = segments.iter().map(Segment::persisted).collect();
                    bridge.downloads.retain_intro(job, stamp);
                }
                FETCHING.store(false, Ordering::Release);
                let _ = crate::app::profiles::invoke_from_event_loop(move || {
                    if bridge.player.screenshot_session() != native {
                        return;
                    }
                    let mut state = bridge.shared.lock().unwrap();
                    if let Some(target) = state
                        .playback
                        .as_mut()
                        .filter(|p| p.session == session && p.intro_skip.native == native)
                    {
                        target.intro_skip.publish(native, segments);
                    }
                });
            });
        }
        let position = app.get_position() as f64;
        let offer = {
            let mut state = self.shared.lock().unwrap();
            let auto = mode == nova_config::IntroSkipMode::Auto && !app.get_is_paused();
            let Some(target) = state.playback.as_mut() else {
                return 0;
            };
            if !compatible_duration(target.intro_skip.duration, duration) {
                return 0;
            }
            target.intro_skip.offer(position, auto, minimum)
        };
        if let Some((kind, end, automatic)) = offer {
            if automatic {
                let _ = self
                    .dispatch_playback_action(nova_playback_api::PlaybackAction::SeekAbsolute(end));
                0
            } else {
                kind
            }
        } else {
            0
        }
    }

    pub(super) fn skip_current_segment(&self) {
        let Some(app) = self.app() else { return };
        let global = self.shared.lock().unwrap().cache_settings.intro_skip_mode;
        if self.player.intro_skip_mode(global) == nova_config::IntroSkipMode::Off
            || !self.intro_skip_eligible(&app)
        {
            return;
        }
        let native = self.player.screenshot_session();
        let end = {
            let mut state = self.shared.lock().unwrap();
            let Some(target) = state
                .playback
                .as_mut()
                .filter(|p| p.intro_skip.native == native)
            else {
                return;
            };
            if !compatible_duration(target.intro_skip.duration, app.get_duration() as f64) {
                return;
            }
            let Some(index) = target.intro_skip.active(app.get_position() as f64) else {
                return;
            };
            target.intro_skip.segments[index].consumed = true;
            target.intro_skip.segments[index].end
        };
        app.set_skip_segment_kind(0);
        let _ = self.dispatch_playback_action(nova_playback_api::PlaybackAction::SeekAbsolute(end));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn anime() -> Lookup {
        Lookup::AniSkip {
            mal: 21,
            episode: 1,
        }
    }
    #[test]
    fn timing_requires_matching_cut_and_excludes_story_mixed_openings() {
        let bytes = br#"{"found":true,"results":[{"skipType":"op","episodeLength":1444,"interval":{"startTime":30,"endTime":120}},{"skipType":"mixed-op","episodeLength":1444,"interval":{"startTime":180,"endTime":220}}]}"#;
        assert_eq!(parse(&anime(), bytes, 1444.0).len(), 1);
        assert!(parse(&anime(), bytes, 1500.0).is_empty());
        assert!(parse(&anime(), b"not json", 1444.0).is_empty());
    }
    #[test]
    fn explicit_seek_and_replay_never_rearm_automatic_skip() {
        let mut session = Session {
            segments: vec![Segment {
                kind: 1,
                start: 10.0,
                end: 90.0,
                consumed: false,
                confidence_percent: None,
            }],
            ..Session::default()
        };
        session.seek(30.0, 0.0);
        assert!(session.segments[0].consumed);
        assert_eq!(session.active(20.0), Some(0)); // manual skip remains available
        assert_eq!(session.active(90.0), None);
    }
    #[test]
    fn dragging_ui_thumb_back_before_delayed_lookup_keeps_replay_manual() {
        let mut session = Session {
            observed: 30.0,
            ..Session::default()
        };
        // Slint has already assigned the destination to position before seek().
        session.seek(0.0, 0.0);
        assert!(session.seek_floor >= 30.0);
        session.publish(
            0,
            vec![Segment {
                kind: 1,
                start: 10.0,
                end: 90.0,
                consumed: false,
                confidence_percent: None,
            }],
        );
        assert!(session.segments[0].consumed);
        assert_eq!(session.active(20.0), Some(0));
    }

    #[test]
    fn replaced_native_source_cannot_publish_old_timing() {
        let mut session = Session {
            native: 8,
            ..Session::default()
        };
        session.publish(
            7,
            vec![Segment {
                kind: 1,
                start: 10.0,
                end: 90.0,
                consumed: false,
                confidence_percent: None,
            }],
        );
        assert!(session.segments.is_empty());
        assert_eq!(session.active(30.0), None);
    }

    #[test]
    fn identities_never_guess_seasonal_anime_or_bare_catalog_ids() {
        let ids = providers::normalize_external_ids(&HashMap::new(), "mal:21", "anime");
        assert!(matches!(
            lookup(&ids, 1, 2),
            Some(Lookup::AniSkip {
                mal: 21,
                episode: 2
            })
        ));
        assert!(lookup(&ids, 2, 2).is_none());
        assert!(lookup(&providers::ExternalIds::default(), 1, 2).is_none());
    }
    #[test]
    fn introdb_requires_confidence_duration_and_finite_early_interval() {
        let request = Lookup::IntroDb {
            key: "imdb_id".into(),
            id: "tt0944947".into(),
            season: 1,
            episode: 1,
        };
        let bytes = br#"{"type":"tv","season":1,"episode":1,"group_duration_ms":1444000,"intro":[{"start_ms":null,"end_ms":90000,"confidence":0.95}]}"#;
        assert_eq!(parse(&request, bytes, 1444.0).len(), 1);
        assert!(parse(&request, bytes, 1500.0).is_empty());
        let low = String::from_utf8(bytes.to_vec())
            .unwrap()
            .replace("0.95", "0.5");
        assert_eq!(
            parse(&request, low.as_bytes(), 1444.0)[0].confidence_percent,
            Some(50)
        );
        for confidence in ["-0.1", "1.1", "null", "\"0.95\""] {
            let invalid = String::from_utf8(bytes.to_vec())
                .unwrap()
                .replace("0.95", confidence);
            assert!(parse(&request, invalid.as_bytes(), 1444.0).is_empty());
        }
    }

    #[test]
    fn confidence_presets_use_inclusive_thresholds_without_hiding_manual_offers() {
        let request = Lookup::IntroDb {
            key: "imdb_id".into(),
            id: "tt0460649".into(),
            season: 1,
            episode: 6,
        };
        for score in [0, 25, 50, 80, 95, 100] {
            let bytes = serde_json::to_vec(&serde_json::json!({"type":"tv", "season":1, "episode":6,
                "group_duration_ms":1444000, "intro":[{"start_ms":30000,"end_ms":120000,"confidence":score as f64 / 100.0}]})).unwrap();
            for index in 0..5 {
                let minimum = nova_config::IntroSkipConfidence::from_index(index)
                    .unwrap()
                    .percent();
                let mut session = Session {
                    segments: parse(&request, &bytes, 1444.0),
                    ..Session::default()
                };
                assert_eq!(session.offer(45.0, false, minimum), Some((1, 120.0, false)));
                assert_eq!(
                    session.offer(45.0, true, minimum),
                    Some((1, 120.0, score >= minimum))
                );
            }
        }
    }

    #[test]
    fn reported_episodes_respect_confidence_presets_online_and_offline() {
        // Actual provider responses for HIMYM S1E6 and Code Geass S2E21.
        // Their timings were previously discarded by the automatic confidence gate.
        for (id, season, episode, duration, start, end, confidence) in [
            (
                "tt0460649",
                1,
                6,
                1330.0,
                Value::Null,
                13987,
                0.5667456894709746,
            ),
            (
                "tt0994314",
                2,
                21,
                1448.0,
                Value::from(25026),
                115102,
                0.5963626933041053,
            ),
        ] {
            let request = Lookup::IntroDb {
                key: "imdb_id".into(),
                id: id.into(),
                season,
                episode,
            };
            let mut body = serde_json::json!({
                "type": "tv", "season": season, "episode": episode,
                "group_duration_ms": duration * 1000.0,
                "intro": [{"start_ms": start, "end_ms": end, "confidence": confidence}]
            });
            let parse_body = |body: &Value, duration| {
                parse(&request, &serde_json::to_vec(body).unwrap(), duration)
            };
            let segments = parse_body(&body, duration);
            assert_eq!(segments.len(), 1);
            let position = segments[0].start + 2.0;
            let expected = Some((1, end as f64 / 1000.0, false));
            let mut session = Session {
                segments,
                ..Session::default()
            };
            assert_eq!(session.offer(position, false, 80), expected);
            assert_eq!(session.offer(position, true, 80), expected);
            session.seek(position, 0.0);
            assert_eq!(session.offer(position, false, 80), expected);

            // A download/cache round trip must not promote this evidence to auto-skip.
            let saved = DownloadIntroTimings {
                duration_ms: (duration * 1000.0) as u64,
                segments: session.segments.iter().map(Segment::persisted).collect(),
                artifact_size: 0,
                artifact_modified_ns: 0,
            };
            let restored = serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
            session.segments = restore_segments(&restored, duration);
            assert_eq!(session.offer(position, true, 80), expected);
            assert_eq!(
                session.offer(
                    position,
                    true,
                    nova_config::IntroSkipConfidence::default().percent()
                ),
                Some((1, end as f64 / 1000.0, true))
            );
            assert_eq!(session.offer(position, true, 0), expected); // once per playback
            assert!(parse_body(&body, duration + 30.0).is_empty());
            body["episode"] = Value::from(episode + 1);
            assert!(parse_body(&body, duration).is_empty());
            body["episode"] = Value::from(episode);
            body["intro"][0]["confidence"] = Value::from(0.8);
            session.segments = parse_body(&body, duration);
            assert_eq!(
                session.offer(position, true, 80),
                Some((1, end as f64 / 1000.0, true))
            );
            assert_eq!(session.offer(position, true, 80), expected);
        }
    }

    #[test]
    fn downloaded_timings_retry_failures_with_backoff_and_stop_on_cancel() {
        let cancel = CancellationToken::new();
        let mut attempts = 0;
        let mut waits = Vec::new();
        let body = br#"{"found":true,"results":[{"skipType":"op","episodeLength":1444,"interval":{"startTime":30,"endTime":120}}]}"#;
        let result = fetch_download_segments(
            &anime(),
            1444.0,
            &cancel,
            |_| {
                attempts += 1;
                (attempts == 3).then(|| body.to_vec())
            },
            |delay| {
                waits.push(delay);
                true
            },
        );
        assert_eq!(attempts, 3);
        assert_eq!(waits, [1, 3]);
        assert_eq!(result.unwrap().len(), 1);

        let mut attempts = 0;
        let result = fetch_download_segments(
            &anime(),
            1444.0,
            &cancel,
            |_| {
                attempts += 1;
                None
            },
            |_| {
                cancel.cancel();
                false
            },
        );
        assert!(result.is_none());
        assert_eq!(attempts, 1);

        let cancel = CancellationToken::new();
        let mut attempts = 0;
        let result = fetch_download_segments(
            &anime(),
            1444.0,
            &cancel,
            |_| {
                attempts += 1;
                Some(br#"{"found":false}"#.to_vec())
            },
            |_| panic!("a missing timing is not a transport failure"),
        );
        assert!(result.is_none());
        assert_eq!(attempts, 1);
    }

    #[test]
    fn offline_timings_validate_duration_and_intervals_again() {
        let mut saved = DownloadIntroTimings {
            duration_ms: 1444000,
            segments: vec![DownloadIntroSegment {
                kind: 1,
                start_ms: 30000,
                end_ms: 120000,
                prompt_only: false,
                confidence_percent: None,
            }],
            artifact_size: 0,
            artifact_modified_ns: 0,
        };
        assert_eq!(restore_segments(&saved, 1444.0).len(), 1);
        saved.segments[0].confidence_percent = Some(101);
        assert!(restore_segments(&saved, 1444.0).is_empty());
        saved.segments[0].confidence_percent = Some(50);
        assert!(restore_segments(&saved, 1500.0).is_empty());
        saved.segments[0].end_ms = 1444000;
        assert!(restore_segments(&saved, 1444.0).is_empty());
        saved.segments[0].end_ms = 120000;
        saved.segments[0].kind = 3;
        assert!(restore_segments(&saved, 1444.0).is_empty());
        assert!(
            Lookup::restored(&DownloadIntroRequest::IntroDb {
                key: "url".into(),
                id: "https://example.test".into(),
                season: 1,
                episode: 1,
            })
            .is_none()
        );
    }

    #[test]
    fn legacy_download_scores_remain_conservative_for_stricter_presets() {
        for (flag, minimum, auto) in [(false, 80, true), (false, 95, false), (true, 50, false)] {
            let raw = serde_json::json!({"duration_ms":1444000,"artifact_size":0,"artifact_modified_ns":0,
                "segments":[{"kind":1,"start_ms":30000,"end_ms":120000,"prompt_only":flag}]});
            let timings = serde_json::from_value(raw).unwrap();
            let mut session = Session {
                segments: restore_segments(&timings, 1444.0),
                ..Session::default()
            };
            assert_eq!(session.offer(45.0, true, minimum), Some((1, 120.0, auto)));
        }
    }
}

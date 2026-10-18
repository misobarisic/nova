//! Align addon subtitles against dialogue already decoded by the playing mpv.
//! A hidden secondary track avoids a second media reader, seeks or downloads.
//! Only a well-supported constant offset is applied; edits/cuts and drift are
//! deliberately left to the viewer rather than guessed from a short sample.
use libmpv2::Mpv;
use std::collections::HashMap;

const RANGE: i32 = 150; // ±15 seconds, in 100 ms steps.
const STEP: f64 = 0.1;
const HISTORY: f64 = 180.0;

#[derive(Clone, Debug)]
struct Cue {
    start: f64,
    end: f64,
    text: String,
}

impl Cue {
    fn read(mpv: &Mpv, secondary: bool) -> Option<Self> {
        let (start, end, text) = if secondary {
            (
                "secondary-sub-start",
                "secondary-sub-end",
                "secondary-sub-text",
            )
        } else {
            ("sub-start", "sub-end", "sub-text")
        };
        let start = mpv.get_property::<f64>(start).ok()?;
        let end = mpv.get_property::<f64>(end).ok()?;
        if !start.is_finite()
            || !end.is_finite()
            || start < 0.0
            || !(0.1..=20.0).contains(&(end - start))
        {
            return None;
        }
        // mpv strips ASS/HTML styling. Bound retained text even for a hostile
        // subtitle file, and ignore punctuation/case for same-language matches.
        let text = mpv.get_property::<String>(text).ok()?;
        let text: String = text
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .take(512)
            .collect();
        if text.is_empty() {
            return None;
        }
        Some(Self { start, end, text })
    }
}

#[derive(Default)]
struct Alignment {
    since: Option<f64>,
    last: Option<f64>,
    checked: f64,
    downloaded: Vec<Cue>,
    embedded: Vec<Cue>,
}

impl Alignment {
    fn observe(&mut self, now: f64, downloaded: Option<Cue>, embedded: Option<Cue>) -> Option<f64> {
        if !now.is_finite() || now < 0.0 {
            return None;
        }
        if self
            .last
            .is_some_and(|last| now < last - 0.5 || now > last + 3.0)
        {
            // A seek must not splice unrelated stretches into matching evidence.
            *self = Self::default();
        }
        self.last = Some(now);
        let since = *self.since.get_or_insert(now);
        for (cues, cue) in [
            (&mut self.downloaded, downloaded),
            (&mut self.embedded, embedded),
        ] {
            cues.retain(|cue| cue.end >= now - HISTORY);
            if let Some(cue) = cue
                && !cues.iter().any(|old| {
                    (old.start - cue.start).abs() < 0.01 && (old.end - cue.end).abs() < 0.01
                })
                && cues.len() < 512
            {
                cues.push(cue);
            }
        }
        if now - self.checked < 2.0 {
            return None;
        }
        self.checked = now;
        self.text_offset().or_else(|| {
            self.timing_offset(
                since.max(now - HISTORY) + RANGE as f64 * STEP,
                now - RANGE as f64 * STEP,
            )
        })
    }

    fn text_offset(&self) -> Option<f64> {
        let mut matches = Vec::new();
        for reference in &self.embedded {
            // Short repeated responses ("yes", "thank you") are weak anchors.
            if reference.text.chars().count() < 12 {
                continue;
            }
            let candidates: Vec<_> = self
                .downloaded
                .iter()
                .filter(|cue| {
                    cue.text == reference.text
                        && (reference.start - cue.start).abs() <= RANGE as f64 * STEP
                })
                .collect();
            if candidates.len() == 1
                && self
                    .embedded
                    .iter()
                    .filter(|cue| cue.text == reference.text)
                    .count()
                    == 1
            {
                matches.push((reference.start, reference.start - candidates[0].start));
            }
        }
        if matches.len() < 5 {
            return None;
        }
        matches.sort_by(|a, b| a.1.total_cmp(&b.1));
        let median = matches[matches.len() / 2].1;
        // Require agreement across the sample, not just a majority around one
        // scene. A changing offset usually means another edit or frame rate.
        if matches
            .iter()
            .any(|(_, offset)| (offset - median).abs() > 0.25)
        {
            return None;
        }
        let first = matches.iter().map(|m| m.0).fold(f64::INFINITY, f64::min);
        let last = matches.iter().map(|m| m.0).fold(0.0, f64::max);
        (last - first >= 20.0).then_some((median / STEP).round() * STEP)
    }

    fn timing_offset(&self, from: f64, to: f64) -> Option<f64> {
        // Padding at both ends keeps unobserved cues from looking like silence
        // when testing positive/negative offsets. This path also works across
        // languages, once about 90 seconds of playback provide enough evidence.
        if to - from < 60.0
            || [&self.downloaded, &self.embedded].iter().any(|cues| {
                cues.iter()
                    .filter(|cue| cue.start >= from && cue.end <= to)
                    .count()
                    < 16
            })
        {
            return None;
        }
        let len = ((to - from) / STEP).floor() as usize;
        let reference = activity(&self.embedded, from, len);
        let input = activity(
            &self.downloaded,
            from - RANGE as f64 * STEP,
            len + 2 * RANGE as usize,
        );
        let scores: Vec<_> = (-RANGE..=RANGE)
            .map(|shift| {
                let begin = (RANGE - shift) as usize;
                let input = &input[begin..begin + len];
                [
                    correlation(&reference, input),
                    correlation(&reference[..len / 2], &input[..len / 2]),
                    correlation(&reference[len / 2..], &input[len / 2..]),
                ]
            })
            .collect();
        let best = |part: usize| {
            scores
                .iter()
                .enumerate()
                .max_by(|a, b| a.1[part].total_cmp(&b.1[part]))
                .map(|(index, _)| index)
                .unwrap()
        };
        let index = best(0);
        let score = scores[index];
        let runner_up = scores
            .iter()
            .enumerate()
            .filter(|(other, _)| other.abs_diff(index) >= 10)
            .map(|(_, score)| score[0])
            .fold(-1.0, f64::max);
        if score[0] < 0.82
            || score[1] < 0.75
            || score[2] < 0.75
            || score[0] - runner_up < 0.12
            || best(1).abs_diff(index) > 3
            || best(2).abs_diff(index) > 3
        {
            return None;
        }
        Some((index as i32 - RANGE) as f64 * STEP)
    }
}

fn activity(cues: &[Cue], from: f64, len: usize) -> Vec<bool> {
    let mut bins = vec![false; len];
    for cue in cues {
        let start = (((cue.start - from) / STEP).round() as isize).clamp(0, len as isize) as usize;
        let end = (((cue.end - from) / STEP).round() as isize).clamp(0, len as isize) as usize;
        if start < end {
            bins[start..end].fill(true);
        }
    }
    bins
}

// Matthews correlation rejects both all-dialogue and all-silence matches. Raw
// overlap would incorrectly reward a nearly continuous subtitle/signs track.
fn correlation(reference: &[bool], input: &[bool]) -> f64 {
    let (mut both, mut neither, mut only_reference, mut only_input) =
        (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    for (&a, &b) in reference.iter().zip(input) {
        match (a, b) {
            (true, true) => both += 1.0,
            (false, false) => neither += 1.0,
            (true, false) => only_reference += 1.0,
            (false, true) => only_input += 1.0,
        }
    }
    let denominator = ((both + only_reference)
        * (both + only_input)
        * (neither + only_reference)
        * (neither + only_input))
        .sqrt();
    if denominator == 0.0 {
        -1.0
    } else {
        (both * neither - only_reference * only_input) / denominator
    }
}

fn selected(mpv: &Mpv, property: &str) -> Option<i32> {
    mpv.get_property::<String>(property).ok()?.parse().ok()
}

fn reference_track(raw: &str, primary: i32) -> Option<i32> {
    use nova_config::playback_preferences::normalize_language;
    let tracks: Vec<serde_json::Value> = serde_json::from_str(raw).ok()?;
    let language = tracks
        .iter()
        .find(|t| t["id"].as_i64() == Some(primary as i64))
        .and_then(|t| t["lang"].as_str())
        .and_then(normalize_language);
    let semantics = super::track_preferences::parse_tracks(raw);
    tracks
        .iter()
        .filter(|t| {
            t["type"] == "sub"
                && t["external"].as_bool() == Some(false)
                && t["id"].as_i64() != Some(primary as i64)
                && matches!(
                    t["codec"].as_str(),
                    Some("ass" | "ssa" | "subrip" | "srt" | "webvtt" | "mov_text" | "text")
                )
                && semantics.iter().any(|track| {
                    Some(track.id as i64) == t["id"].as_i64()
                        && track.descriptor.kind
                            == Some(nova_config::playback_preferences::TrackKind::Full)
                })
        })
        .min_by_key(|t| {
            let same_language =
                language.is_some() && t["lang"].as_str().and_then(normalize_language) == language;
            (
                !same_language,
                t["hearing-impaired"].as_bool().unwrap_or(false),
                !t["default"].as_bool().unwrap_or(false),
            )
        })
        .and_then(|t| t["id"].as_i64())
        .and_then(|id| i32::try_from(id).ok())
}

struct Reference {
    id: i32,
    selection: String,
    visibility: bool,
    delay: f64,
}

impl Reference {
    fn start(mpv: &Mpv, id: i32) -> Option<Self> {
        // Never commandeer a secondary track chosen by the viewer or a script.
        if selected(mpv, "secondary-sid").is_some() {
            return None;
        }
        let reference = Self {
            id,
            selection: mpv.get_property("secondary-sid").ok()?,
            visibility: mpv.get_property("secondary-sub-visibility").ok()?,
            delay: mpv.get_property("secondary-sub-delay").ok()?,
        };
        let result = mpv
            .set_property("secondary-sub-visibility", false)
            .and_then(|()| mpv.set_property("secondary-sub-delay", 0.0_f64))
            .and_then(|()| mpv.set_property("secondary-sid", id.to_string()));
        if result.is_err() {
            let _ = mpv.set_property("secondary-sub-visibility", reference.visibility);
            let _ = mpv.set_property("secondary-sub-delay", reference.delay);
            return None;
        }
        Some(reference)
    }

    fn release(self, mpv: &Mpv) {
        let current = selected(mpv, "secondary-sid");
        if current == Some(self.id) {
            let _ = mpv.set_property("secondary-sid", self.selection);
        }
        // Selecting the reference as primary may clear secondary-sid itself.
        // Still restore our hidden visibility unless another track took over.
        if current.is_none() || current == Some(self.id) {
            let _ = mpv.set_property("secondary-sub-visibility", self.visibility);
            let _ = mpv.set_property("secondary-sub-delay", self.delay);
        }
    }
}

#[derive(Default)]
struct Track {
    offset: f64,
    finished: bool,
}

#[derive(Default)]
pub(super) struct SubtitleSync {
    tracks: HashMap<i32, Track>,
    active: Option<i32>,
    reference: Option<Reference>,
    alignment: Alignment,
}

impl SubtitleSync {
    pub(super) fn register(&mut self, id: i32) {
        self.tracks.entry(id).or_default();
    }

    #[cfg(any(target_os = "android", test))]
    pub(super) fn remap(&mut self, old: i32, new: i32) {
        let track = self.tracks.remove(&old);
        // Recovery only re-adds the selected addon. Every other old numeric ID
        // could now belong to an embedded track in the replacement track list.
        self.tracks.clear();
        if let Some(track) = track {
            self.tracks.insert(new, track);
        }
        self.active = None;
        self.reference = None;
        self.alignment = Alignment::default();
    }

    #[cfg(any(target_os = "android", test))]
    pub(super) fn suspend(&mut self, mpv: &Mpv, restore: Option<i32>) {
        if let Some(reference) = self.reference.take() {
            reference.release(mpv);
        }
        self.active = None;
        self.tracks.retain(|id, _| Some(*id) == restore);
        self.alignment = Alignment::default();
    }

    pub(super) fn clear(&mut self, mpv: Option<&Mpv>) {
        if let Some(reference) = self.reference.take()
            && let Some(mpv) = mpv
        {
            reference.release(mpv);
        }
        *self = Self::default();
    }

    pub(super) fn release_reference(&mut self, mpv: &Mpv) {
        if let Some(reference) = self.reference.take() {
            reference.release(mpv);
        }
        self.alignment = Alignment::default();
    }

    pub(super) fn reset_timing(&mut self, mpv: &Mpv, manual: bool) {
        self.release_reference(mpv);
        if let Some(track) = selected(mpv, "sid").and_then(|id| self.tracks.get_mut(&id)) {
            track.offset = 0.0;
            track.finished = manual;
        }
    }

    /// Never persist the measured correction as a title/global preference.
    /// An explicit timing edit replaces it with the viewer's absolute delay.
    pub(super) fn delay(&self, mpv: &Mpv, base: f64) -> f64 {
        base + selected(mpv, "sid")
            .and_then(|id| self.tracks.get(&id))
            .map_or(0.0, |track| track.offset)
    }

    pub(super) fn poll(&mut self, mpv: &Mpv, base: f64, manual_timing: bool) {
        let primary = selected(mpv, "sid");
        let active = primary.filter(|id| self.tracks.contains_key(id));
        if self.active != active {
            if let Some(reference) = self.reference.take() {
                reference.release(mpv);
            }
            self.alignment = Alignment::default();
            if self.active.is_some() || active.is_some() {
                let _ = mpv.set_property("sub-delay", self.delay(mpv, base));
            }
            self.active = active;
        }
        let Some(id) = active else { return };
        if manual_timing || base.abs() > 0.001 || self.tracks[&id].finished {
            if let Some(reference) = self.reference.take() {
                reference.release(mpv);
            }
            self.alignment = Alignment::default();
            return;
        }
        if self.reference.is_none() {
            let Some(reference) = mpv
                .get_property::<String>("track-list")
                .ok()
                .and_then(|raw| reference_track(&raw, id))
                .and_then(|id| Reference::start(mpv, id))
            else {
                return;
            };
            self.reference = Some(reference);
        }
        if selected(mpv, "secondary-sid") != self.reference.as_ref().map(|reference| reference.id) {
            if let Some(reference) = self.reference.take() {
                reference.release(mpv);
            }
            self.tracks.get_mut(&id).unwrap().finished = true;
            return;
        }
        let Some(now) = mpv.get_property::<f64>("time-pos").ok() else {
            return;
        };
        if let Some(offset) =
            self.alignment
                .observe(now, Cue::read(mpv, false), Cue::read(mpv, true))
        {
            let track = self.tracks.get_mut(&id).unwrap();
            if mpv.set_property("sub-delay", base + offset).is_ok() {
                track.offset = offset;
                track.finished = true;
            }
            if let Some(reference) = self.reference.take() {
                reference.release(mpv);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cues(seed: u64, offset: f64, same_text: bool) -> Vec<Cue> {
        let mut random = seed;
        let mut time = 0.0;
        (0..60)
            .map(|index| {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                time += 0.4 + (random >> 32) as u32 as f64 % 27.0 / 10.0;
                let duration = 0.8 + (random >> 16) as u32 as f64 % 22.0 / 10.0;
                let cue = Cue {
                    start: time + offset,
                    end: time + duration + offset,
                    text: format!(
                        "{}dialogueline{index}",
                        if same_text { "same" } else { "translated" }
                    ),
                };
                time += duration;
                cue
            })
            .collect()
    }

    #[test]
    fn subtitle_sync_matches_distinct_text_with_both_offset_signs() {
        for offset in [-4.3, 3.2] {
            let alignment = Alignment {
                downloaded: cues(7, offset, true),
                embedded: cues(7, 0.0, true),
                ..Default::default()
            };
            assert!((alignment.text_offset().unwrap() + offset).abs() < 0.001);
        }
    }

    #[test]
    fn subtitle_sync_matches_translations_by_timing_and_tolerates_missing_cues() {
        for offset in [-4.3, 3.2, 0.0] {
            let embedded = cues(7, 0.0, true);
            let mut downloaded = cues(7, offset, false);
            // Translations may omit a cue and have slightly different endings.
            downloaded.remove(21);
            for cue in &mut downloaded {
                cue.end += 0.1;
            }
            let alignment = Alignment {
                downloaded,
                embedded,
                ..Default::default()
            };
            assert!(alignment.text_offset().is_none());
            let result = alignment.timing_offset(20.0, 160.0);
            assert!(
                result.is_some_and(|result| (result + offset).abs() < 0.11),
                "offset {offset}: {result:?}"
            );
        }
    }

    #[test]
    fn subtitle_sync_rejects_wrong_edits_drift_and_sparse_or_ambiguous_evidence() {
        let reference = cues(7, 0.0, true);
        let mut different_edit = cues(7, 3.0, true);
        for cue in &mut different_edit[25..] {
            cue.start += 2.0;
            cue.end += 2.0;
        }
        let mut drift = cues(7, 3.0, true);
        for cue in &mut drift {
            cue.start *= 1.04;
            cue.end *= 1.04;
        }
        let periodic = |offset| {
            (0..60)
                .map(|n| Cue {
                    start: n as f64 * 4.0 + offset,
                    end: n as f64 * 4.0 + offset + 2.0,
                    text: "yes".into(),
                })
                .collect::<Vec<_>>()
        };
        let cases = [
            (reference.clone(), cues(41, 0.0, false)),
            (reference.clone(), different_edit),
            (reference.clone(), drift),
            (reference[..4].to_vec(), reference[..4].to_vec()),
            (periodic(0.0), periodic(2.0)),
        ];
        for (embedded, downloaded) in cases {
            let alignment = Alignment {
                embedded,
                downloaded,
                ..Default::default()
            };
            assert!(alignment.text_offset().is_none());
            assert!(alignment.timing_offset(20.0, 160.0).is_none());
        }
        assert_eq!(correlation(&[true; 20], &[true; 20]), -1.0);
        assert_eq!(correlation(&[false; 20], &[false; 20]), -1.0);
    }

    #[test]
    fn subtitle_sync_collects_live_evidence_and_discards_it_after_seeks() {
        let downloaded = cues(7, 3.2, false);
        let embedded = cues(7, 0.0, true);
        let mut alignment = Alignment::default();
        let mut correction = None;
        for tick in 0..720 {
            let now = tick as f64 / 4.0;
            let current = |cues: &[Cue]| {
                cues.iter()
                    .find(|cue| cue.start <= now && cue.end > now)
                    .cloned()
            };
            correction = alignment.observe(now, current(&downloaded), current(&embedded));
            if correction.is_some() {
                break;
            }
        }
        assert!(correction.is_some_and(|offset| (offset + 3.2).abs() < 0.11));
        assert!(alignment.observe(500.0, None, None).is_none());
        assert!(alignment.downloaded.is_empty());
        assert!(alignment.embedded.is_empty());
        assert!(alignment.observe(20.0, None, None).is_none());
        assert_eq!(alignment.since, Some(20.0));
    }

    #[test]
    fn subtitle_sync_uses_only_embedded_full_text_and_prefers_same_language() {
        let tracks = serde_json::json!([
            {"id": 8, "type": "sub", "external": true, "codec": "subrip", "lang": "hr"},
            {"id": 1, "type": "sub", "external": false, "codec": "subrip", "lang": "en", "default": true},
            {"id": 2, "type": "sub", "external": false, "codec": "ass", "lang": "hrv"},
            {"id": 3, "type": "sub", "external": false, "codec": "subrip", "lang": "hr", "forced": true},
            {"id": 4, "type": "sub", "external": false, "codec": "hdmv_pgs_subtitle", "lang": "hr"},
            {"id": 5, "type": "sub", "external": false, "codec": "ass", "lang": "hr", "title": "Signs & Songs"},
            {"id": 6, "type": "sub", "external": true, "codec": "subrip", "lang": "hr"}
        ]);
        assert_eq!(reference_track(&tracks.to_string(), 8), Some(2));
        assert_eq!(reference_track("[]", 8), None);
        assert_eq!(
            reference_track(
                r#"[{"id":1,"type":"sub","external":true,"codec":"subrip"}]"#,
                8
            ),
            None
        );
    }

    #[test]
    fn subtitle_sync_recovery_drops_obsolete_track_ids() {
        let mut sync = SubtitleSync::default();
        sync.register(8);
        sync.register(9);
        sync.tracks.get_mut(&8).unwrap().offset = -3.0;
        sync.tracks.get_mut(&8).unwrap().finished = true;
        sync.remap(8, 4);
        assert_eq!(sync.tracks.len(), 1);
        assert_eq!(sync.tracks[&4].offset, -3.0);
        assert!(sync.tracks[&4].finished);
    }

    #[test]
    fn subtitle_sync_native_reference_is_hidden_and_keeps_media_pause_and_selection() {
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
                "nova-subtitle-sync-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )));
        std::fs::create_dir(&scratch.0).unwrap();
        let audio = scratch.0.join("audio.wav");
        let size = 80_000u32;
        let mut wav = b"RIFF".to_vec();
        wav.extend((size + 36).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(8000u32.to_le_bytes());
        wav.extend(8000u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(8u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(size.to_le_bytes());
        wav.resize(wav.len() + size as usize, 128);
        std::fs::write(&audio, wav).unwrap();
        let reference = scratch.0.join("reference.srt");
        let downloaded = scratch.0.join("downloaded.srt");
        std::fs::write(
            &reference,
            "1\n00:00:01,000 --> 00:00:03,000\nEmbedded reference dialogue\n",
        )
        .unwrap();
        std::fs::write(
            &downloaded,
            "1\n00:00:04,000 --> 00:00:06,000\nDownloaded dialogue\n",
        )
        .unwrap();
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("config", false)?;
            init.set_property("vo", "null")?;
            init.set_property("ao", "null")?;
            init.set_property("pause", true)
        })
        .unwrap();
        mpv.command("loadfile", &[&audio.to_string_lossy()])
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while mpv.get_property::<f64>("duration").is_err() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // A local sidecar exercises the same native secondary decoder; the
        // metadata test above separately proves sidecars aren't auto-selected.
        mpv.command("sub-add", &[&reference.to_string_lossy(), "select"])
            .unwrap();
        let reference = selected(&mpv, "sid").unwrap();
        mpv.command("sub-add", &[&downloaded.to_string_lossy(), "select"])
            .unwrap();
        let downloaded = selected(&mpv, "sid").unwrap();
        let path = mpv.get_property::<String>("path").unwrap();
        let mut sync = SubtitleSync::default();
        sync.register(downloaded);
        sync.poll(&mpv, 0.0, false);
        assert!(sync.reference.is_none()); // No embedded track: leave it alone.
        mpv.set_property("sub-delay", -3.0_f64).unwrap();
        sync.tracks.get_mut(&downloaded).unwrap().offset = -3.0;
        sync.reference = Reference::start(&mpv, reference);
        assert!(sync.reference.is_some());
        assert!(
            !mpv.get_property::<bool>("secondary-sub-visibility")
                .unwrap()
        );
        assert_eq!(selected(&mpv, "sid"), Some(downloaded));
        mpv.command("seek", &["2", "absolute+exact"]).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let (Some(a), Some(b)) = (Cue::read(&mpv, false), Cue::read(&mpv, true)) {
                assert_eq!(a.start, 4.0); // Raw cue time excludes sub-delay.
                assert_eq!(b.start, 1.0);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "hidden subtitle decoder produced no cue"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(sync.delay(&mpv, 0.2), -2.8);
        // The hidden reference must become selectable as primary immediately.
        sync.release_reference(&mpv);
        mpv.set_property("sid", reference.to_string()).unwrap();
        assert_eq!(selected(&mpv, "sid"), Some(reference));
        sync.poll(&mpv, 0.0, false);
        assert_eq!(mpv.get_property::<f64>("sub-delay").unwrap(), 0.0);
        mpv.set_property("sid", downloaded.to_string()).unwrap();
        sync.poll(&mpv, 0.0, false);
        assert_eq!(mpv.get_property::<f64>("sub-delay").unwrap(), -3.0);
        sync.reference = Reference::start(&mpv, reference);
        assert!(sync.reference.is_some());
        assert!(Reference::start(&mpv, reference).is_none());
        // Editing the displayed -3.0 to -2.8 must save/apply -2.8 absolutely,
        // without persisting an invisible +0.2 delta or adding -3.0 twice.
        sync.reset_timing(&mpv, true);
        assert_eq!(sync.delay(&mpv, -2.8), -2.8);
        mpv.set_property("sub-delay", -2.8_f64).unwrap();
        sync.poll(&mpv, -2.8, true);
        assert!(sync.tracks[&downloaded].finished);
        assert!(sync.reference.is_none());
        assert!(
            mpv.get_property::<bool>("secondary-sub-visibility")
                .unwrap()
        );
        assert_eq!(selected(&mpv, "secondary-sid"), None);
        mpv.set_property("sid", reference.to_string()).unwrap();
        sync.poll(&mpv, 0.0, false);
        assert_eq!(mpv.get_property::<f64>("sub-delay").unwrap(), 0.0);
        assert_eq!(mpv.get_property::<String>("path").unwrap(), path);
        assert!(mpv.get_property::<bool>("pause").unwrap());
        sync.suspend(&mpv, None);
        sync.clear(Some(&mpv));
        assert!(sync.tracks.is_empty());
    }
}

impl super::Player {
    pub(super) fn poll_subtitle_sync(&self) {
        if self.state.pending.lock().unwrap().is_some()
            || !self.state.started.load(super::Ordering::SeqCst)
        {
            return;
        }
        let engine = self.state.mpv.lock().unwrap();
        let Some(mpv) = engine.as_ref() else { return };
        if mpv.get_property::<String>("path").ok().as_deref()
            != Some(self.state.preference_source.lock().unwrap().as_str())
        {
            return;
        }
        let preferences = self.state.preferences.lock().unwrap();
        let mut style = self.resolve_subtitle_style(preferences.preferences.subtitle_style());
        let manual = preferences
            .preferences
            .title
            .subtitle_style
            .is_some_and(|style| style.delay.is_some());
        drop(preferences);
        let mut sync = self.state.subtitle_sync.lock().unwrap();
        sync.poll(mpv, style.delay as f64, manual);
        style.delay = sync.delay(mpv, style.delay as f64) as f32;
        drop(sync);
        drop(engine);
        // The delay editor shows the measured result, so a later manual nudge
        // saves the absolute value rather than a correction-dependent delta.
        use slint::Model;
        if let Some(app) = self.state.app.upgrade()
            && app.get_effective_subtitle_style().row_data(0) != Some(style.delay)
        {
            app.set_effective_subtitle_style(slint::ModelRc::new(slint::VecModel::from(
                style.values().to_vec(),
            )));
        }
    }
}

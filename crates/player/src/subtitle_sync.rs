//! Align addon subtitles against dialogue already decoded by the playing mpv.
//! Both visible slots can share a trusted reference; a hidden secondary track
//! is used only while that slot is free. No second media reader or scanning.
//! Only a well-supported constant offset is applied; edits/cuts and drift are
//! deliberately left to the viewer rather than guessed from a short sample.
use libmpv2::Mpv;
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

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

#[derive(Clone, Default)]
struct Alignment {
    since: Option<f64>,
    last: Option<f64>,
    checked: f64,
    downloaded: Vec<Cue>,
    embedded: Vec<Cue>,
}

impl Alignment {
    fn collect(&mut self, now: f64, downloaded: Option<Cue>, embedded: Option<Cue>) -> bool {
        if !now.is_finite() || now < 0.0 {
            return false;
        }
        if self
            .last
            .is_some_and(|last| now < last - 0.5 || now > last + 3.0)
        {
            // A seek must not splice unrelated stretches into matching evidence.
            *self = Self::default();
        }
        self.last = Some(now);
        self.since.get_or_insert(now);
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
            return false;
        }
        self.checked = now;
        true
    }

    fn estimate(&self, now: f64) -> Option<f64> {
        self.text_offset().or_else(|| {
            self.timing_offset(
                self.since.unwrap_or(now).max(now - HISTORY) + RANGE as f64 * STEP,
                now - RANGE as f64 * STEP,
            )
        })
    }

    #[cfg(test)]
    fn observe(&mut self, now: f64, downloaded: Option<Cue>, embedded: Option<Cue>) -> Option<f64> {
        self.collect(now, downloaded, embedded)
            .then(|| self.estimate(now))
            .flatten()
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

#[derive(Default)]
struct Inventory {
    raw: String,
    full_embedded: Vec<i32>,
    references: HashMap<i32, i32>,
}
impl Inventory {
    fn parse(raw: &str) -> Self {
        use nova_config::playback_preferences::{TrackKind, normalize_language};
        let tracks: Vec<serde_json::Value> = serde_json::from_str(raw).unwrap_or_default();
        let semantics = super::track_preferences::parse_tracks(raw);
        let anchors = tracks
            .iter()
            .filter(|t| {
                t["type"] == "sub"
                    && t["external"].as_bool() == Some(false)
                    && matches!(
                        t["codec"].as_str(),
                        Some("ass" | "ssa" | "subrip" | "srt" | "webvtt" | "mov_text" | "text")
                    )
                    && semantics.iter().any(|track| {
                        Some(track.id as i64) == t["id"].as_i64()
                            && track.descriptor.kind == Some(TrackKind::Full)
                    })
            })
            .take(64)
            .collect::<Vec<_>>();
        let mut inventory = Self {
            raw: raw.into(),
            ..Self::default()
        };
        inventory.full_embedded = anchors
            .iter()
            .filter_map(|t| t["id"].as_i64().and_then(|id| i32::try_from(id).ok()))
            .collect();
        for track in semantics.iter().filter(|track| !track.audio).take(512) {
            if let Some(reference) = anchors
                .iter()
                .filter(|t| t["id"].as_i64() != Some(track.id as i64))
                .min_by_key(|t| {
                    let same = track.descriptor.language.is_some()
                        && t["lang"].as_str().and_then(normalize_language)
                            == track.descriptor.language;
                    (
                        !same,
                        t["hearing-impaired"].as_bool().unwrap_or(false),
                        !t["default"].as_bool().unwrap_or(false),
                    )
                })
                .and_then(|t| t["id"].as_i64())
                .and_then(|id| i32::try_from(id).ok())
            {
                inventory.references.insert(track.id, reference);
            }
        }
        inventory
    }
}
#[cfg(test)]
fn reference_track(raw: &str, primary: i32) -> Option<i32> {
    Inventory::parse(raw).references.get(&primary).copied()
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) enum Slot {
    #[default]
    Primary,
    Secondary,
}
impl Slot {
    fn index(self) -> usize {
        if self == Self::Primary { 0 } else { 1 }
    }
    fn other(self) -> Self {
        if self == Self::Primary {
            Self::Secondary
        } else {
            Self::Primary
        }
    }
    fn selection(self) -> &'static str {
        if self == Self::Primary {
            "sid"
        } else {
            "secondary-sid"
        }
    }
    fn delay(self) -> &'static str {
        if self == Self::Primary {
            "sub-delay"
        } else {
            "secondary-sub-delay"
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Anchor {
    id: i32,
    slot: Option<Slot>,
    delay: u64,
}
#[derive(Clone, Default)]
struct Track {
    offset: f64,
    finished: bool,
    confident: bool,
    manual: bool,
    anchor: Option<Anchor>,
    cached_revision: Option<u64>,
    alignment: Alignment,
}
impl Track {
    fn bind_anchor(&mut self, anchor: Anchor) {
        // A visible/hidden reference can become a cached reference when the
        // viewer fills the other slot. Its identity and effective timestamps
        // have not changed: retain the addon cues already overlapping it.
        if self
            .anchor
            .is_none_or(|old| old.id != anchor.id || old.delay != anchor.delay)
        {
            self.alignment = Alignment::default();
            self.cached_revision = None;
        }
        self.anchor = Some(anchor);
    }
}

#[derive(Default)]
struct Evidence {
    cues: Vec<Cue>,
    delay: u64,
    since: f64,
    revision: u64,
}
impl Evidence {
    fn collect(&mut self, now: f64, delay: f64, cue: Option<Cue>) {
        if self.delay != delay.to_bits() || self.cues.is_empty() {
            self.cues.clear();
            self.delay = delay.to_bits();
            self.since = now;
            self.revision = self.revision.wrapping_add(1);
        }
        let old_len = self.cues.len();
        self.cues.retain(|cue| cue.end >= now - HISTORY);
        if old_len != self.cues.len() {
            self.revision = self.revision.wrapping_add(1);
        }
        if let Some(cue) = cue
            && self.cues.len() < 512
            && !self
                .cues
                .iter()
                .any(|old| (old.start - cue.start).abs() < 0.01 && (old.end - cue.end).abs() < 0.01)
        {
            self.cues.push(cue);
            self.revision = self.revision.wrapping_add(1);
        }
    }
}
struct EstimateJob {
    generation: u64,
    slot: Slot,
    id: i32,
    anchor: Anchor,
    expected_delay: f64,
    result: mpsc::Receiver<Option<f64>>,
}
impl EstimateJob {
    fn current(
        &self,
        generation: u64,
        selected: Option<i32>,
        anchor: Option<Anchor>,
        actual_delay: Option<f64>,
        eligible: bool,
    ) -> bool {
        eligible
            && self.generation == generation
            && selected == Some(self.id)
            && anchor == Some(self.anchor)
            && actual_delay.is_some_and(|delay| {
                delay.is_finite() && (delay - self.expected_delay).abs() <= 0.001
            })
    }
}
#[cfg(any(target_os = "android", test))]
#[derive(Clone, Copy, Debug)]
pub(super) struct TrackRemap {
    pub slot: Slot,
    pub old: i32,
    pub new: i32,
}

#[derive(Default)]
pub(super) struct SubtitleSync {
    // `tracks` is also the source-local registry of addon IDs. Secondary-only
    // manual records never enter it, so an embedded ID cannot become an addon.
    tracks: HashMap<i32, Track>,
    secondary_tracks: HashMap<i32, Track>,
    suspended: Option<(HashMap<i32, Track>, HashMap<i32, Track>)>,
    active: [Option<i32>; 2],
    expected: [Option<f64>; 2],
    reference: Option<Reference>,
    inventory: Option<Inventory>,
    evidence: HashMap<i32, Evidence>,
    last_position: Option<f64>,
    generation: u64,
    job: Option<EstimateJob>,
    busy: Arc<AtomicBool>,
    next_slot: Slot,
}

fn shifted(mut cue: Cue, delay: f64) -> Cue {
    // mpv's cue properties exclude sub-delay/secondary-sub-delay.
    cue.start += delay;
    cue.end += delay;
    cue
}

impl SubtitleSync {
    fn records(&self, slot: Slot) -> &HashMap<i32, Track> {
        if slot == Slot::Primary {
            &self.tracks
        } else {
            &self.secondary_tracks
        }
    }
    fn records_mut(&mut self, slot: Slot) -> &mut HashMap<i32, Track> {
        if slot == Slot::Primary {
            &mut self.tracks
        } else {
            &mut self.secondary_tracks
        }
    }
    fn invalidate_work(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.job = None;
    }
    fn clear_evidence(&mut self) {
        self.evidence.clear();
        self.invalidate_work();
        for track in self
            .tracks
            .values_mut()
            .chain(self.secondary_tracks.values_mut())
        {
            track.alignment = Alignment::default();
            track.anchor = None;
        }
    }
    pub(super) fn update_inventory(&mut self, raw: &str) {
        if self
            .inventory
            .as_ref()
            .is_none_or(|inventory| inventory.raw != raw)
        {
            self.inventory = Some(Inventory::parse(raw));
        }
    }
    pub(super) fn register(&mut self, id: i32) {
        if self.tracks.len() >= 64
            && !self.tracks.contains_key(&id)
            && let Some(old) = self
                .tracks
                .keys()
                .copied()
                .find(|old| !self.active.contains(&Some(*old)))
        {
            self.tracks.remove(&old);
            self.secondary_tracks.remove(&old);
        }
        self.tracks.entry(id).or_default();
        self.secondary_tracks.entry(id).or_default();
        self.inventory = None;
        self.invalidate_work();
    }

    #[cfg(any(target_os = "android", test))]
    pub(super) fn remap_pair(&mut self, mapping: &[TrackRemap]) {
        // Detach both old maps before inserting ANY new ID. A swapped ID or an
        // ID reused by an embedded recovered track must never consume a partner.
        let (primary, secondary) = self.suspended.take().unwrap_or_else(|| {
            (
                std::mem::take(&mut self.tracks),
                std::mem::take(&mut self.secondary_tracks),
            )
        });
        self.tracks.clear();
        self.secondary_tracks.clear();
        for remap in mapping {
            if let Some(track) = primary.get(&remap.old) {
                self.tracks.insert(remap.new, track.clone());
            }
            if let Some(track) = secondary.get(&remap.old) {
                self.secondary_tracks.insert(remap.new, track.clone());
            }
            self.active[remap.slot.index()] = None;
        }
        self.active = [None, None];
        self.expected = [None, None];
        self.inventory = None;
        self.last_position = None;
        self.clear_evidence();
    }
    #[cfg(test)]
    fn remap(&mut self, old: i32, new: i32) {
        self.remap_pair(&[TrackRemap {
            slot: Slot::Primary,
            old,
            new,
        }]);
    }

    #[cfg(any(target_os = "android", test))]
    pub(super) fn suspend_pair(&mut self, mpv: &Mpv, restore: [Option<i32>; 2]) {
        self.release_reference(mpv);
        if self.suspended.is_none() {
            let mut primary = std::mem::take(&mut self.tracks);
            let mut secondary = std::mem::take(&mut self.secondary_tracks);
            primary.retain(|id, _| restore.contains(&Some(*id)));
            secondary.retain(|id, _| restore.contains(&Some(*id)));
            self.suspended =
                (!primary.is_empty() || !secondary.is_empty()).then_some((primary, secondary));
        }
        self.active = [None, None];
        self.expected = [None, None];
        self.inventory = None;
        self.last_position = None;
        self.clear_evidence();
    }
    #[cfg(test)]
    pub(super) fn suspend(&mut self, mpv: &Mpv, restore: Option<i32>) {
        self.suspend_pair(mpv, [restore, None]);
    }

    pub(super) fn clear(&mut self, mpv: Option<&Mpv>) {
        if let Some(reference) = self.reference.take()
            && let Some(mpv) = mpv
        {
            reference.release(mpv);
        }
        // Native slot delays survive loadfile. A closed source must not lend
        // its secondary correction to an unrelated stream or an Off selection.
        if let Some(mpv) = mpv {
            let _ = mpv.set_property("secondary-sub-delay", 0.0_f64);
        }
        // An obsolete worker owns only bounded data. Keep its busy lease across
        // source changes so a second expensive estimator cannot overlap it.
        let busy = self.busy.clone();
        *self = Self::default();
        self.busy = busy;
    }
    pub(super) fn owns_reference(&self, mpv: &Mpv) -> bool {
        self.reference
            .as_ref()
            .is_some_and(|reference| selected(mpv, "secondary-sid") == Some(reference.id))
    }
    pub(super) fn release_reference(&mut self, mpv: &Mpv) {
        if let Some(reference) = self.reference.take() {
            reference.release(mpv);
            self.invalidate_work();
        }
    }
    pub(super) fn reset_timing(&mut self, mpv: &Mpv, manual: bool) {
        self.release_reference(mpv);
        self.invalidate_work();
        if let Some(track) = selected(mpv, "sid").and_then(|id| self.tracks.get_mut(&id)) {
            track.offset = 0.0;
            track.finished = manual;
            track.manual = manual;
            track.confident = false;
            track.alignment = Alignment::default();
            track.anchor = None;
        }
        self.expected[0] = None;
    }
    pub(super) fn delay(&self, mpv: &Mpv, base: f64) -> f64 {
        base + selected(mpv, "sid")
            .and_then(|id| self.tracks.get(&id))
            .map_or(0.0, |track| track.offset)
    }
    pub(super) fn secondary_selection_changed(&mut self, mpv: &Mpv) {
        if self.owns_reference(mpv) {
            return;
        }
        let id = selected(mpv, Slot::Secondary.selection());
        if self.active[1] != id {
            if let Some(old) = self.active[1].and_then(|id| self.secondary_tracks.get_mut(&id)) {
                old.alignment = Alignment::default();
                old.anchor = None;
                old.cached_revision = None;
            }
            let delay = id
                .and_then(|id| self.secondary_tracks.get(&id))
                .map_or(0.0, |track| track.offset);
            if mpv.set_property("secondary-sub-delay", delay).is_ok() {
                self.expected[1] = Some(delay);
            }
            self.active[1] = id;
            self.invalidate_work();
        }
    }
    pub(super) fn primary_selection_changed(&mut self, mpv: &Mpv, base: f64) {
        let primary = selected(mpv, Slot::Primary.selection());
        if self.active[0] != primary {
            if let Some(old) = self.active[0].and_then(|id| self.tracks.get_mut(&id)) {
                old.alignment = Alignment::default();
                old.anchor = None;
                old.cached_revision = None;
            }
            self.release_reference(mpv);
            if self.active[0].is_some() || primary.is_some_and(|id| self.tracks.contains_key(&id)) {
                let delay = self.delay(mpv, base);
                if mpv.set_property("sub-delay", delay).is_ok() {
                    self.expected[0] = Some(delay);
                }
            }
            self.active[0] = primary;
            self.invalidate_work();
        }
    }
    fn selections(&mut self, mpv: &Mpv, base: f64) {
        self.primary_selection_changed(mpv, base);
        self.secondary_selection_changed(mpv);
        for slot in [Slot::Primary, Slot::Secondary] {
            if slot == Slot::Secondary && self.owns_reference(mpv) {
                continue;
            }
            let actual = mpv
                .get_property::<f64>(slot.delay())
                .ok()
                .filter(|delay| delay.is_finite());
            if let (Some(id), Some(expected), Some(actual)) = (
                self.active[slot.index()],
                self.expected[slot.index()],
                actual,
            ) && (actual - expected).abs() > 0.001
            {
                if slot == Slot::Primary && !self.tracks.contains_key(&id) {
                    self.expected[0] = Some(actual);
                    self.invalidate_work();
                    continue;
                }
                let track = self.records_mut(slot).entry(id).or_default();
                track.offset = actual - if slot == Slot::Primary { base } else { 0.0 };
                track.finished = true;
                track.manual = true;
                track.confident = false;
                track.alignment = Alignment::default();
                track.anchor = None;
                self.expected[slot.index()] = Some(actual);
                self.invalidate_work();
            } else if self.expected[slot.index()].is_none() {
                self.expected[slot.index()] = actual;
            }
        }
    }
    fn anchor(
        &self,
        slot: Slot,
        id: i32,
        selected: [Option<i32>; 2],
        delays: [f64; 2],
        now: f64,
    ) -> Option<Anchor> {
        let other = slot.other();
        if let Some(reference) = selected[other.index()].filter(|reference| *reference != id) {
            let embedded = self
                .inventory
                .as_ref()
                .is_some_and(|inventory| inventory.full_embedded.contains(&reference));
            let trusted = self
                .records(other)
                .get(&reference)
                .is_some_and(|track| track.confident && !track.manual);
            if embedded || trusted {
                return Some(Anchor {
                    id: reference,
                    slot: Some(other),
                    delay: delays[other.index()].to_bits(),
                });
            }
        }
        self.evidence
            .iter()
            .filter(|(_, evidence)| evidence.cues.iter().any(|cue| cue.end >= now - HISTORY))
            .max_by_key(|(_, evidence)| evidence.cues.len())
            .map(|(id, evidence)| Anchor {
                id: *id,
                slot: None,
                delay: evidence.delay,
            })
    }
    fn start_estimate(
        &mut self,
        slot: Slot,
        id: i32,
        anchor: Anchor,
        expected_delay: f64,
        now: f64,
    ) {
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let alignment = self.records(slot)[&id].alignment.clone();
        let (send, result) = mpsc::sync_channel(1);
        let busy = self.busy.clone();
        let spawned = std::thread::Builder::new()
            .name("subtitle-align".into())
            .spawn(move || {
                struct Lease(Arc<AtomicBool>);
                impl Drop for Lease {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                let _lease = Lease(busy);
                let _ = send.send(alignment.estimate(now));
            });
        if spawned.is_err() {
            self.busy.store(false, Ordering::Release);
            return;
        }
        self.job = Some(EstimateJob {
            generation: self.generation,
            slot,
            id,
            anchor,
            expected_delay,
            result,
        });
        self.next_slot = slot.other();
    }
    fn publish_estimate(&mut self, mpv: &Mpv, anchors: [Option<Anchor>; 2]) {
        let Some(job) = self.job.as_ref() else { return };
        let result = match job.result.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        let job = self.job.take().unwrap();
        if !job.current(
            self.generation,
            self.active[job.slot.index()],
            anchors[job.slot.index()],
            mpv.get_property::<f64>(job.slot.delay()).ok(),
            self.records(job.slot)
                .get(&job.id)
                .is_some_and(|track| !track.finished && !track.manual),
        ) {
            return;
        }
        let Some(track) = self
            .records_mut(job.slot)
            .get_mut(&job.id)
            .filter(|track| !track.manual && !track.finished)
        else {
            return;
        };
        if let Some(offset) = result.filter(|offset| offset.is_finite())
            && mpv.set_property(job.slot.delay(), offset).is_ok()
        {
            track.offset = offset;
            track.finished = true;
            track.confident = true;
            track.alignment = Alignment::default();
            track.anchor = None;
            track.cached_revision = None;
            self.expected[job.slot.index()] = Some(offset);
            if job.slot == Slot::Primary {
                self.release_reference(mpv);
            }
        }
    }
    pub(super) fn poll(&mut self, mpv: &Mpv, base: f64, manual_timing: bool) {
        if self.suspended.is_some() {
            return;
        }
        if self.tracks.is_empty() {
            return;
        }
        self.selections(mpv, base);
        if mpv.get_property::<bool>("pause").unwrap_or(true) {
            return;
        }
        let Some(now) = mpv
            .get_property::<f64>("time-pos")
            .ok()
            .filter(|now| now.is_finite() && *now >= 0.0)
        else {
            return;
        };
        if self
            .last_position
            .is_some_and(|last| now < last - 0.5 || now > last + 3.0)
        {
            self.clear_evidence();
        }
        self.last_position = Some(now);
        if self.inventory.is_none()
            && let Ok(raw) = mpv.get_property::<String>("track-list")
        {
            self.update_inventory(&raw);
        }
        if self.reference.is_some() && !self.owns_reference(mpv) {
            self.reference = None;
            self.invalidate_work();
        }
        let eligible = |this: &Self, slot: Slot| {
            this.active[slot.index()].is_some_and(|id| {
                this.tracks.contains_key(&id)
                    && this
                        .records(slot)
                        .get(&id)
                        .is_some_and(|track| !track.finished && !track.manual)
                    && (slot != Slot::Primary || (!manual_timing && base.abs() <= 0.001))
            })
        };
        if !eligible(self, Slot::Primary) {
            self.release_reference(mpv);
        }
        if !eligible(self, Slot::Primary) && !eligible(self, Slot::Secondary) {
            if self.job.is_some() {
                self.invalidate_work();
            }
            return;
        }
        if eligible(self, Slot::Primary)
            && self.active[1].is_none()
            && self.reference.is_none()
            && let Some(reference) = self
                .inventory
                .as_ref()
                .and_then(|inventory| inventory.references.get(&self.active[0]?).copied())
                .and_then(|id| Reference::start(mpv, id))
        {
            self.reference = Some(reference);
        }
        // Only embedded full-dialogue cues enter the reusable bounded cache.
        for slot in [Slot::Primary, Slot::Secondary] {
            let id = if slot == Slot::Secondary {
                selected(mpv, "secondary-sid")
            } else {
                self.active[0]
            };
            if let Some(id) = id.filter(|id| {
                self.inventory
                    .as_ref()
                    .is_some_and(|inventory| inventory.full_embedded.contains(id))
            }) {
                let delay = mpv.get_property::<f64>(slot.delay()).unwrap_or(0.0);
                if self.evidence.contains_key(&id) || self.evidence.len() < 8 {
                    self.evidence.entry(id).or_default().collect(
                        now,
                        delay,
                        Cue::read(mpv, slot == Slot::Secondary).map(|cue| shifted(cue, delay)),
                    );
                }
            }
        }
        let delays = [
            mpv.get_property::<f64>("sub-delay").unwrap_or(base),
            mpv.get_property::<f64>("secondary-sub-delay")
                .unwrap_or(0.0),
        ];
        let mut visible = self.active;
        if self.owns_reference(mpv) {
            visible[1] = None;
        }
        let anchors = [
            self.active[0].and_then(|id| self.anchor(Slot::Primary, id, visible, delays, now)),
            self.active[1].and_then(|id| self.anchor(Slot::Secondary, id, visible, delays, now)),
        ];
        if self
            .job
            .as_ref()
            .is_some_and(|job| !eligible(self, job.slot))
        {
            self.invalidate_work();
        }
        self.publish_estimate(mpv, anchors);
        let order = [self.next_slot, self.next_slot.other()];
        for slot in order {
            if !eligible(self, slot) {
                continue;
            }
            let id = self.active[slot.index()].unwrap();
            let Some(anchor) = anchors[slot.index()] else {
                continue;
            };
            let target = Cue::read(mpv, slot == Slot::Secondary);
            let reference = anchor
                .slot
                .and_then(|slot| Cue::read(mpv, slot == Slot::Secondary))
                .map(|cue| shifted(cue, f64::from_bits(anchor.delay)));
            let cached = anchor
                .slot
                .is_none()
                .then(|| self.evidence.get(&anchor.id))
                .flatten();
            let record = &self.records(slot)[&id];
            let cached_cues = cached
                .filter(|evidence| {
                    record.anchor != Some(anchor)
                        || record.cached_revision != Some(evidence.revision)
                })
                .map(|evidence| (evidence.cues.clone(), evidence.since, evidence.revision));
            let track = self.records_mut(slot).get_mut(&id).unwrap();
            track.bind_anchor(anchor);
            if let Some((cues, since, revision)) = cached_cues {
                track.alignment.embedded = cues;
                track.alignment.since.get_or_insert(since);
                track.cached_revision = Some(revision);
            }
            let ready = track.alignment.collect(now, target, reference);
            if ready && self.job.is_none() {
                self.start_estimate(slot, id, anchor, delays[slot.index()], now);
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
    fn visible_reference_slots_use_their_applied_delay_and_never_guess_between_untrusted_addons() {
        let mut sync = SubtitleSync::default();
        sync.register(8);
        sync.register(9);
        sync.update_inventory(
            r#"[
            {"id":1,"type":"sub","external":false,"codec":"subrip","lang":"en"},
            {"id":2,"type":"sub","external":false,"codec":"subrip","lang":"hr"},
            {"id":8,"type":"sub","external":true,"codec":"subrip","lang":"en"},
            {"id":9,"type":"sub","external":true,"codec":"subrip","lang":"hr"}
        ]"#,
        );
        let primary = sync
            .anchor(Slot::Secondary, 9, [Some(1), Some(9)], [1.4, 0.0], 100.0)
            .unwrap();
        assert_eq!(primary.slot, Some(Slot::Primary));
        assert_eq!(f64::from_bits(primary.delay), 1.4);
        let secondary = sync
            .anchor(Slot::Primary, 8, [Some(8), Some(2)], [0.0, -0.7], 100.0)
            .unwrap();
        assert_eq!(secondary.slot, Some(Slot::Secondary));
        assert_eq!(f64::from_bits(secondary.delay), -0.7);
        for slot in [Slot::Primary, Slot::Secondary] {
            assert!(
                sync.anchor(
                    slot,
                    if slot == Slot::Primary { 8 } else { 9 },
                    [Some(8), Some(9)],
                    [0.0, 0.0],
                    100.0
                )
                .is_none()
            );
        }
        sync.tracks.get_mut(&8).unwrap().confident = true;
        let anchor = sync
            .anchor(Slot::Secondary, 9, [Some(8), Some(9)], [-3.2, 0.0], 100.0)
            .unwrap();
        assert_eq!(anchor.id, 8);
        let alignment = Alignment {
            downloaded: cues(7, 4.7, true),
            embedded: cues(7, 3.2, true)
                .into_iter()
                .map(|cue| shifted(cue, -3.2))
                .collect(),
            ..Default::default()
        };
        assert!(
            (alignment.text_offset().unwrap() + 4.7).abs() < 0.001,
            "secondary measures its own -4.7 correction, never copies primary -3.2"
        );
        let shifted_embedded = Alignment {
            downloaded: cues(7, 3.2, true),
            embedded: cues(7, 0.0, true)
                .into_iter()
                .map(|cue| shifted(cue, 1.4))
                .collect(),
            ..Default::default()
        };
        assert!(
            (shifted_embedded.text_offset().unwrap() + 1.8).abs() < 0.001,
            "manual delay on embedded primary must not disable secondary or omit reference delay"
        );
    }

    #[test]
    fn hidden_reference_evidence_survives_filling_both_slots_but_not_seek_or_source_reset() {
        let mut sync = SubtitleSync::default();
        sync.register(8);
        sync.register(9);
        let mut evidence = Evidence::default();
        for cue in cues(7, 0.0, true) {
            evidence.collect(cue.start, 0.0, Some(cue));
        }
        assert!(evidence.cues.len() <= 512);
        sync.evidence.insert(2, evidence);
        let anchor = sync
            .anchor(Slot::Secondary, 9, [Some(8), Some(9)], [0.0, 0.0], 120.0)
            .unwrap();
        assert_eq!(anchor.slot, None);
        assert_eq!(anchor.id, 2);
        assert!(
            sync.anchor(Slot::Secondary, 9, [Some(8), Some(9)], [0.0, 0.0], 1000.0)
                .is_none()
        );
        sync.clear_evidence();
        assert!(
            sync.anchor(Slot::Secondary, 9, [Some(8), Some(9)], [0.0, 0.0], 120.0)
                .is_none()
        );
        sync.clear(None);
        assert!(sync.tracks.is_empty());
        assert!(sync.secondary_tracks.is_empty());
    }

    #[test]
    fn filling_reference_slot_preserves_overlapping_addon_evidence_for_cached_alignment() {
        let visible = Anchor {
            id: 2,
            slot: Some(Slot::Secondary),
            delay: 0.0_f64.to_bits(),
        };
        let mut track = Track::default();
        track.bind_anchor(visible);
        track.alignment = Alignment {
            downloaded: cues(7, 3.2, true),
            embedded: cues(7, 0.0, true),
            ..Default::default()
        };
        let before = track.alignment.downloaded.len();
        track.bind_anchor(Anchor {
            slot: None,
            ..visible
        });
        assert_eq!(track.alignment.downloaded.len(), before);
        assert!((track.alignment.text_offset().unwrap() + 3.2).abs() < 0.001);
        track.bind_anchor(Anchor {
            delay: 1.4_f64.to_bits(),
            ..visible
        });
        assert!(
            track.alignment.downloaded.is_empty(),
            "changing effective reference timing needs new evidence"
        );
    }

    #[test]
    fn estimator_result_rejects_replaced_generation_track_anchor_manual_delay_and_manual_control() {
        let (_, result) = mpsc::sync_channel(1);
        let anchor = Anchor {
            id: 1,
            slot: Some(Slot::Primary),
            delay: 1.4_f64.to_bits(),
        };
        let job = EstimateJob {
            generation: 7,
            slot: Slot::Secondary,
            id: 9,
            anchor,
            expected_delay: 0.0,
            result,
        };
        assert!(job.current(7, Some(9), Some(anchor), Some(0.0), true));
        assert!(!job.current(8, Some(9), Some(anchor), Some(0.0), true)); // seek/source/selection generation
        assert!(!job.current(7, Some(10), Some(anchor), Some(0.0), true));
        assert!(!job.current(
            7,
            Some(9),
            Some(Anchor {
                delay: 0.0_f64.to_bits(),
                ..anchor
            }),
            Some(0.0),
            true
        ));
        assert!(!job.current(7, Some(9), Some(anchor), Some(1.7), true));
        assert!(!job.current(7, Some(9), Some(anchor), Some(f64::NAN), true));
        assert!(!job.current(7, Some(9), Some(anchor), Some(0.0), false));
    }

    #[test]
    fn source_reset_keeps_one_estimator_lease_and_pair_remap_cannot_alias_partner() {
        let mut sync = SubtitleSync::default();
        let busy = sync.busy.clone();
        busy.store(true, Ordering::Release);
        sync.clear(None);
        assert!(Arc::ptr_eq(&busy, &sync.busy));
        sync.register(8);
        sync.register(9);
        sync.register(10);
        let anchor = Anchor {
            id: 1,
            slot: None,
            delay: 0.0_f64.to_bits(),
        };
        sync.start_estimate(Slot::Primary, 8, anchor, 0.0, 100.0);
        assert!(
            sync.job.is_none(),
            "obsolete running worker prevents another estimator"
        );
        busy.store(false, Ordering::Release);
        sync.tracks.get_mut(&8).unwrap().offset = -3.2;
        sync.tracks.get_mut(&8).unwrap().confident = true;
        sync.secondary_tracks.get_mut(&9).unwrap().offset = 1.7;
        sync.secondary_tracks.get_mut(&9).unwrap().manual = true;
        sync.remap_pair(&[
            TrackRemap {
                slot: Slot::Primary,
                old: 8,
                new: 9,
            },
            TrackRemap {
                slot: Slot::Secondary,
                old: 9,
                new: 8,
            },
        ]);
        assert_eq!(sync.tracks.len(), 2);
        assert!(!sync.tracks.contains_key(&10));
        assert_eq!(sync.tracks[&9].offset, -3.2);
        assert!(sync.tracks[&9].confident);
        assert_eq!(sync.secondary_tracks[&8].offset, 1.7);
        assert!(sync.secondary_tracks[&8].manual);
        assert_eq!(sync.secondary_tracks[&9].offset, 0.0);
        assert_eq!(sync.active, [None, None]);
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
        // An unrelated audio switch refreshes track metadata while automatic
        // alignment owns the hidden secondary slot. Visible-secondary Off must
        // leave that reference selected and invisible, so matching continues.
        let old_audio = mpv.get_property::<String>("aid").unwrap();
        mpv.command("audio-add", &[&audio.to_string_lossy(), "select"])
            .unwrap();
        assert_ne!(mpv.get_property::<String>("aid").unwrap(), old_audio);
        super::super::secondary_subtitles::apply_secondary_track(
            &mpv,
            &mut sync,
            &nova_config::playback_preferences::TrackChoice::Off,
            -1,
        );
        sync.poll(&mpv, 0.0, false);
        assert!(sync.owns_reference(&mpv));
        assert!(
            !mpv.get_property::<bool>("secondary-sub-visibility")
                .unwrap()
        );
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
        mpv.set_property("sid", downloaded.to_string()).unwrap();
        sync.poll(&mpv, 0.0, false);
        sync.register(reference);
        mpv.set_property("secondary-sid", reference.to_string())
            .unwrap();
        sync.secondary_selection_changed(&mpv);
        mpv.set_property("secondary-sub-delay", 1.7_f64).unwrap();
        sync.selections(&mpv, 0.0);
        assert!(sync.secondary_tracks[&reference].manual);
        assert!((sync.secondary_tracks[&reference].offset - 1.7).abs() < 1e-6);
        mpv.set_property("sid", "no").unwrap();
        mpv.set_property("secondary-sid", downloaded.to_string())
            .unwrap();
        sync.secondary_selection_changed(&mpv);
        assert_eq!(
            mpv.get_property::<f64>("secondary-sub-delay").unwrap(),
            0.0,
            "secondary manual delay cannot leak to a different addon"
        );
        mpv.set_property("secondary-sid", reference.to_string())
            .unwrap();
        sync.secondary_selection_changed(&mpv);
        assert!((mpv.get_property::<f64>("secondary-sub-delay").unwrap() - 1.7).abs() < 1e-6);
        mpv.set_property("secondary-sid", "no").unwrap();
        sync.secondary_selection_changed(&mpv);
        assert_eq!(mpv.get_property::<f64>("secondary-sub-delay").unwrap(), 0.0);
        sync.suspend(&mpv, None);
        mpv.set_property("secondary-sub-delay", 3.0_f64).unwrap();
        sync.clear(Some(&mpv));
        assert!(sync.tracks.is_empty());
        assert_eq!(
            mpv.get_property::<f64>("secondary-sub-delay").unwrap(),
            0.0,
            "closing a source clears its native secondary delay"
        );
    }
}

impl super::Player {
    pub(super) fn poll_subtitle_sync(&self) {
        if self.state.pending.lock().unwrap().is_some()
            || !self.state.started.load(super::Ordering::SeqCst)
        {
            return;
        }
        // The cached UI pause state avoids every engine/property query while
        // menus animate over paused video. Immediate selection hooks apply
        // existing offsets; an estimator result can safely wait until resume.
        let Some(app) = self.state.app.upgrade() else {
            return;
        };
        if app.get_is_paused() {
            return;
        }
        {
            let sync = self.state.subtitle_sync.lock().unwrap();
            if sync.tracks.is_empty() || sync.suspended.is_some() {
                return;
            }
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
        if let Ok(raw) = self.state.track_inventory(mpv) {
            sync.update_inventory(&raw);
        }
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

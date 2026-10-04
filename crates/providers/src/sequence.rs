//! Provider-neutral identity and episode alignment. Site parsing stays in JS;
//! counts, title anchors and ambiguity rules are shared by every provider.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::{MediaItem, StreamLookupRequest};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSequence {
    pub media_id: String,
    pub title: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub year: Option<String>,
    /// Explicit source labels override title-derived season/part labels.
    pub season: Option<u32>,
    pub part: Option<u32>,
    pub declared_count: Option<u32>,
    pub episodes: Vec<SequenceEpisode>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SequenceEpisode {
    pub id: String,
    pub number: u32,
    pub title: String,
    pub released: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeResolution {
    pub status: String,
    pub source_episode_id: Option<String>,
    pub source_media_id: Option<String>,
    pub number: Option<u32>,
}

impl EpisodeResolution {
    fn empty(status: &str) -> Self {
        Self {
            status: status.into(),
            source_episode_id: None,
            source_media_id: None,
            number: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SeriesIdentity {
    pub base: String,
    pub season: u32,
    pub part: u32,
    pub labeled: bool,
}

pub(crate) fn normalized_title(value: &str) -> String {
    let mut result = String::new();
    let mut space = false;
    for c in value
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
    {
        if c.is_alphanumeric() {
            if space && !result.is_empty() {
                result.push(' ');
            }
            result.push(c);
            space = false;
        } else {
            space = true;
        }
    }
    result
}

pub(crate) fn episode_title(value: &str) -> String {
    static PREFIX: OnceLock<Regex> = OnceLock::new();
    let normalized = normalized_title(value);
    let title = PREFIX
        .get_or_init(|| Regex::new(r"^(?:episode|ep|stage|turn)\s*\d+(?:\s*\d+)?\s*").unwrap())
        .replace(&normalized, "")
        .into_owned();
    if title.is_empty()
        || title.chars().all(|c| c.is_ascii_digit())
        || ["episode", "ep", "unknown", "untitled", "tba", "tbd"].contains(&title.as_str())
    {
        String::new()
    } else {
        title
    }
}

pub(crate) fn start_year(value: Option<&str>) -> Option<u32> {
    static YEAR: OnceLock<Regex> = OnceLock::new();
    YEAR.get_or_init(|| Regex::new(r"\b(?:19|20)\d{2}\b").unwrap())
        .find(value?)
        .and_then(|m| m.as_str().parse().ok())
}

// Include alignment changes in durable metadata fingerprints so a confirmed
// but partial old mapping cannot hide improvements for thirty days.
pub(crate) const MAPPING_VERSION: u32 = 2;

pub(crate) fn series_identity(value: &str) -> SeriesIdentity {
    static PART: OnceLock<Regex> = OnceLock::new();
    static SEASON: OnceLock<Regex> = OnceLock::new();
    let mut base = normalized_title(value);
    let part_re = PART.get_or_init(|| {
        Regex::new(r"\s+(?:(?:part|cour)\s+(\d+)|(\d+)(?:st|nd|rd|th)\s+(?:part|cour))(?:\s|$)")
            .unwrap()
    });
    let mut part = 1;
    let mut labeled = false;
    if let Some(caps) = part_re.captures(&base) {
        part = caps
            .get(1)
            .or_else(|| caps.get(2))
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(1);
        base.truncate(caps.get(0).unwrap().start());
        labeled = true;
    }
    // Explicit labels may precede subtitles; shorthand/Roman suffixes stay
    // end-anchored to avoid stripping numerals belonging to the series name.
    let season_re = SEASON.get_or_init(|| Regex::new(r"\s+(?:(?:(?:season|series)\s+(\d+)|(\d+)(?:st|nd|rd|th)\s+season|(first|second|third|fourth|fifth|sixth)\s+season)(?:\s|$)|r(\d+)$|(ii|iii|iv|v|vi)$)").unwrap());
    let mut season = 1;
    if let Some(caps) = season_re.captures(&base) {
        season = caps
            .get(1)
            .or_else(|| caps.get(2))
            .or_else(|| caps.get(4))
            .and_then(|m| m.as_str().parse().ok())
            .or_else(|| {
                caps.get(3).and_then(|m| {
                    ["first", "second", "third", "fourth", "fifth", "sixth"]
                        .iter()
                        .position(|w| *w == m.as_str())
                        .map(|i| i as u32 + 1)
                })
            })
            .or_else(|| {
                caps.get(5).and_then(|m| {
                    ["i", "ii", "iii", "iv", "v", "vi"]
                        .iter()
                        .position(|w| *w == m.as_str())
                        .map(|i| i as u32 + 1)
                })
            })
            .unwrap_or(1);
        base.truncate(caps.get(0).unwrap().start());
        labeled = true;
    }
    SeriesIdentity {
        base,
        season,
        part,
        labeled,
    }
}

pub(crate) fn title_relation(expected: &str, titles: &[String]) -> u32 {
    if titles.iter().any(|t| t == expected) {
        2
    } else if expected.len() >= 4
        && titles.iter().any(|t| {
            t.starts_with(&format!("{expected} ")) || expected.starts_with(&format!("{t} "))
        })
    {
        1
    } else {
        0
    }
}

pub(crate) fn candidate_rank(source: &MediaItem, candidate: &MediaItem) -> u32 {
    if source.media_type != candidate.media_type {
        return 0;
    }
    // ID conflicts must never be hidden by a title match.
    if crate::matching::ids_conflict(source, candidate) {
        return 0;
    }
    if crate::match_metadata(source, std::slice::from_ref(candidate))
        == crate::MetadataMatch::Exact(0)
    {
        return 3;
    }
    // Season/cour suffixes describe episodic layout. Movie sequels must keep
    // their full title unless an explicit shared ID establishes identity.
    if !matches!(source.media_type.as_str(), "series" | "anime") {
        return 0;
    }
    let identity = series_identity(&source.title);
    let year = start_year(source.year.as_deref());
    let candidate_year = start_year(candidate.year.as_deref());
    if year.is_some_and(|year| {
        candidate_year.is_none_or(|other| {
            if identity.labeled {
                other > year
            } else {
                other != year
            }
        })
    }) {
        return 0;
    }
    let titles = std::iter::once(&source.title)
        .chain(&source.aliases)
        .map(|s| series_identity(s).base)
        .collect::<Vec<_>>();
    std::iter::once(&candidate.title)
        .chain(&candidate.aliases)
        .map(|s| title_relation(&series_identity(s).base, &titles))
        .max()
        .unwrap_or(0)
}

struct Candidate<'a> {
    source: &'a SourceSequence,
    identity: SeriesIdentity,
    relation: u32,
    base_relation: u32,
    count: Option<u32>,
    confirmed_season: Option<u32>,
    episode_index: BTreeMap<u32, &'a SequenceEpisode>,
    title_index: BTreeMap<String, Vec<u32>>,
}

impl Candidate<'_> {
    fn episode(&self, number: u32) -> Option<&SequenceEpisode> {
        self.episode_index.get(&number).copied()
    }
    fn title_numbers(&self, title: &str) -> Vec<u32> {
        self.title_index.get(title).cloned().unwrap_or_default()
    }
    fn year(&self) -> Option<u32> {
        start_year(self.source.year.as_deref())
    }
}

fn candidates<'a>(title: &str, sources: &'a [SourceSequence]) -> Vec<Candidate<'a>> {
    let expected = normalized_title(title);
    let identity = series_identity(title);
    sources
        .iter()
        .take(6)
        .map(|source| {
            let titles = std::iter::once(&source.title)
                .chain(&source.aliases)
                .map(|t| normalized_title(t))
                .collect::<Vec<_>>();
            let mut own = series_identity(&source.title);
            if let Some(season) = source.season {
                own.season = season;
                own.labeled |= season != 1;
            }
            if let Some(part) = source.part {
                own.part = part;
                own.labeled |= part != 1;
            }
            let numbers = source
                .episodes
                .iter()
                .map(|e| e.number)
                .collect::<BTreeSet<_>>();
            let maximum = numbers.last().copied().unwrap_or(0);
            let count = (maximum > 0
                && maximum <= 10_000
                && numbers.len() == source.episodes.len()
                && numbers.len() == maximum as usize)
                .then_some(source.declared_count.unwrap_or(maximum))
                .filter(|n| *n >= maximum && *n <= 10_000);
            let episode_index = source
                .episodes
                .iter()
                .take(10_000)
                .map(|e| (e.number, e))
                .collect();
            let mut title_index: BTreeMap<String, Vec<u32>> = BTreeMap::new();
            for e in source.episodes.iter().take(10_000) {
                let title = episode_title(&e.title);
                if !title.is_empty() {
                    title_index.entry(title).or_default().push(e.number);
                }
            }
            Candidate {
                source,
                episode_index,
                title_index,
                identity: own,
                relation: title_relation(&expected, &titles),
                base_relation: title_relation(
                    &identity.base,
                    &titles
                        .iter()
                        .map(|t| series_identity(t).base)
                        .collect::<Vec<_>>(),
                ),
                count,
                confirmed_season: None,
            }
        })
        .collect()
}

fn aligned_episode<'a>(
    lookup: &StreamLookupRequest,
    candidates: &'a [Candidate<'a>],
) -> Option<(&'a Candidate<'a>, u32)> {
    if series_identity(&lookup.title).labeled {
        return None;
    }
    let mut ordered = candidates
        .iter()
        .filter(|c| c.base_relation == 2)
        .collect::<Vec<_>>();
    ordered.sort_by_key(|c| (c.identity.season, c.identity.part));
    let first = ordered.first()?;
    if first.identity.season != 1
        || first.identity.part != 1
        || start_year(lookup.year.as_deref()).is_some_and(|y| first.year().is_some_and(|f| f != y))
    {
        return None;
    }
    let mut total = 0_u32;
    let mut boundaries = BTreeSet::from([0]);
    let mut offsets = Vec::new();
    for (i, c) in ordered.iter().enumerate() {
        if i > 0 {
            let previous = ordered[i - 1];
            let same = c.identity.season == previous.identity.season
                && c.identity.part == previous.identity.part + 1;
            let next = c.identity.season == previous.identity.season + 1 && c.identity.part == 1;
            if (!same && !next) || c.year().zip(previous.year()).is_some_and(|(a, b)| a < b) {
                return None;
            }
        }
        offsets.push(total);
        total = total.checked_add(c.count?)?;
        boundaries.insert(total);
    }
    let absolute = lookup.absolute_episode?;
    let count = lookup.season_episode_count?;
    let start = absolute.checked_sub(lookup.episode)?;
    let end = start.checked_add(count)?;
    if count < lookup.episode
        || absolute > total
        || end > total
        || !((boundaries.contains(&start) && boundaries.contains(&end))
            || lookup.series_episode_count == Some(total))
    {
        return None;
    }
    ordered.into_iter().zip(offsets).find_map(|(c, offset)| {
        (absolute > offset && absolute <= offset + c.count?).then_some((c, absolute - offset))
    })
}

fn resolve(lookup: &StreamLookupRequest, candidates: &[Candidate<'_>]) -> EpisodeResolution {
    if lookup.media_type != "series"
        || !(1..=100).contains(&lookup.season)
        || !(1..=10_000).contains(&lookup.episode)
        || lookup.absolute_episode.is_some_and(|n| n < lookup.episode)
        || lookup
            .season_episode_count
            .is_some_and(|n| n < lookup.episode)
        || series_identity(&lookup.title).base.is_empty()
    {
        return EpisodeResolution::empty("missing");
    }
    let source_year = start_year(lookup.year.as_deref());
    let release_year = start_year(lookup.released.as_deref());
    let expected_episode = episode_title(lookup.episode_title.as_deref().unwrap_or_default());
    let eligible = |c: &Candidate<'_>| {
        !source_year.zip(c.year()).is_some_and(|(a, b)| b < a)
            && !release_year.zip(c.year()).is_some_and(|(a, b)| b > a)
    };
    let mut matches: BTreeMap<String, (u32, String, u32)> = BTreeMap::new();
    let mut add = |c: &Candidate<'_>, number: u32, confidence: u32, aligned: bool| {
        if !eligible(c) {
            return;
        }
        let Some(episode) = c.episode(number) else {
            return;
        };
        let actual = episode_title(&episode.title);
        if !expected_episode.is_empty()
            && !actual.is_empty()
            && expected_episode != actual
            && !aligned
        {
            return;
        }
        let key = episode.id.clone();
        if matches
            .get(&key)
            .is_none_or(|(score, _, _)| *score < confidence)
        {
            matches.insert(key, (confidence, c.source.media_id.clone(), number));
        }
    };
    let title_matches = if expected_episode.is_empty() {
        vec![]
    } else {
        candidates
            .iter()
            .filter(|c| eligible(c))
            .flat_map(|c| {
                c.title_numbers(&expected_episode)
                    .into_iter()
                    .map(move |n| (c, n))
            })
            .collect::<Vec<_>>()
    };
    if title_matches.len() == 1 {
        add(title_matches[0].0, title_matches[0].1, 1000, false);
    }
    if let Some((c, n)) = aligned_episode(lookup, candidates) {
        add(c, n, 800, true);
    }
    for c in candidates {
        if lookup.season_episode_count.is_some() && c.count != lookup.season_episode_count {
            continue;
        }
        let same_start = !source_year.zip(c.year()).is_some_and(|(a, b)| a != b);
        let mut number = lookup.episode;
        let mut continuous = false;
        if lookup.season == 1 {
            if !same_start
                || (source_year.is_none()
                    && release_year.zip(c.year()).is_some_and(|(a, b)| a != b))
            {
                continue;
            }
        } else if c.identity.season != lookup.season {
            if c.relation != 2
                || lookup.absolute_episode.is_none_or(|n| n <= lookup.episode)
                || !same_start
            {
                continue;
            }
            number = lookup.absolute_episode.unwrap();
            continuous = true;
        }
        let same_episode = !expected_episode.is_empty()
            && c.episode(number)
                .is_some_and(|e| episode_title(&e.title) == expected_episode);
        let confirmed = !continuous
            && c.confirmed_season == Some(lookup.season)
            && c.count == lookup.season_episode_count;
        if c.relation < 2
            && c.base_relation < 2
            && !(same_episode && title_matches.len() == 1)
            && !confirmed
        {
            continue;
        }
        let score = if confirmed {
            750
        } else {
            (if continuous { 400 } else { 500 })
                + if same_episode { 150 } else { 0 }
                + if c.relation == 2 { 50 } else { 0 }
                + if source_year.is_some() && c.year() == source_year {
                    50
                } else {
                    0
                }
        };
        add(c, number, score, confirmed);
    }
    let mut matches = matches
        .into_iter()
        .map(|(id, (score, media, number))| (score, id, media, number))
        .collect::<Vec<_>>();
    matches.sort_by_key(|m| std::cmp::Reverse(m.0));
    let Some((score, id, media, number)) = matches.first() else {
        return EpisodeResolution::empty("missing");
    };
    if matches.get(1).is_some_and(|m| m.0 == *score) {
        return EpisodeResolution::empty("ambiguous");
    }
    EpisodeResolution {
        status: "confirmed".into(),
        source_episode_id: Some(id.clone()),
        source_media_id: Some(media.clone()),
        number: Some(*number),
    }
}

pub fn resolve_episode(
    lookup: &StreamLookupRequest,
    sources: &[SourceSequence],
) -> EpisodeResolution {
    resolve(lookup, &candidates(&lookup.title, sources))
}

/// Map canonical episodes using the exact same forward resolver, with stronger
/// positional anchors only when two globally unique titles prove a season.
pub(crate) fn map_episodes(
    media: &MediaItem,
    videos: &[addons::Video],
    sources: &[SourceSequence],
) -> BTreeMap<String, String> {
    let mut candidates = candidates(&media.title, sources);
    let mut seasons: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    for video in videos.iter().take(10_000) {
        if let (Some(s), Some(e)) = (video.season, video.episode_number())
            && (1..=100).contains(&s)
            && (1..=10_000).contains(&e)
        {
            seasons.entry(s).or_default().insert(e);
        }
    }
    let counts = seasons
        .iter()
        .filter_map(|(s, ns)| {
            ns.last()
                .filter(|last| ns.len() == **last as usize)
                .map(|last| (*s, *last))
        })
        .collect::<BTreeMap<_, _>>();
    let mut offsets = BTreeMap::new();
    let mut total = Some(0_u32);
    for s in 1..=seasons.keys().next_back().copied().unwrap_or(0) {
        let Some(next) = total
            .zip(counts.get(&s))
            .and_then(|(n, c)| n.checked_add(*c))
        else {
            total = None;
            break;
        };
        offsets.insert(s, total.unwrap());
        total = Some(next);
    }
    let mut canonical_titles: BTreeMap<String, usize> = BTreeMap::new();
    let mut source_titles: BTreeMap<String, usize> = BTreeMap::new();
    for video in videos {
        let t = episode_title(&video.label());
        if !t.is_empty() {
            *canonical_titles.entry(t).or_default() += 1;
        }
    }
    for c in &candidates {
        for e in &c.source.episodes {
            let t = episode_title(&e.title);
            if !t.is_empty() {
                *source_titles.entry(t).or_default() += 1;
            }
        }
    }
    for c in &mut candidates {
        let s = c.identity.season;
        if c.count.is_none() || c.count != counts.get(&s).copied() {
            continue;
        }
        let mut anchors = BTreeSet::new();
        let mut conflict = false;
        for v in videos.iter().filter(|v| v.season == Some(s)) {
            let t = episode_title(&v.label());
            if t.is_empty()
                || canonical_titles.get(&t) != Some(&1)
                || source_titles.get(&t) != Some(&1)
            {
                continue;
            }
            if let Some(n) = c.title_numbers(&t).first() {
                if Some(*n) == v.episode_number() {
                    anchors.insert(t);
                } else {
                    conflict = true;
                }
            }
        }
        if anchors.len() >= 2 && !conflict {
            c.confirmed_season = Some(s);
        }
    }
    let mut mapped: BTreeMap<String, Option<String>> = BTreeMap::new();
    for v in videos.iter().take(10_000) {
        let (Some(season), Some(episode)) = (v.season, v.episode_number()) else {
            continue;
        };
        if v.id.is_empty() || !seasons.get(&season).is_some_and(|ns| ns.contains(&episode)) {
            continue;
        }
        let lookup = StreamLookupRequest {
            media_id: media.source_id.clone(),
            media_type: media.media_type.clone(),
            title: media.title.clone(),
            year: media.year.clone(),
            season,
            episode,
            absolute_episode: offsets.get(&season).and_then(|n| n.checked_add(episode)),
            season_episode_count: counts.get(&season).copied(),
            series_episode_count: total,
            episode_title: Some(v.label()),
            released: v.released.clone(),
        };
        let match_ = resolve(&lookup, &candidates);
        if let Some(native) = match_.source_episode_id {
            mapped
                .entry(native)
                .and_modify(|old| {
                    if old.as_deref() != Some(&v.id) {
                        *old = None;
                    }
                })
                .or_insert_with(|| Some(v.id.clone()));
        }
    }
    mapped
        .into_iter()
        .filter_map(|(id, target)| target.map(|target| (id, target)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn season_and_cour_labels_before_subtitles_identify_the_series_family() {
        for (title, season, part) in [
            ("Solo Leveling Season 2: Arise from the Shadow", 2, 1),
            ("Solo Leveling 2nd Season - Arise from the Shadow", 2, 1),
            ("Solo Leveling Second Season: Arise from the Shadow", 2, 1),
            ("Solo Leveling Season 2 Part 2: A New Arc", 2, 2),
            ("Solo Leveling Season 2 2nd Cour: A New Arc", 2, 2),
        ] {
            let identity = series_identity(title);
            assert_eq!(identity.base, "solo leveling", "{title}");
            assert_eq!((identity.season, identity.part), (season, part), "{title}");
            assert!(identity.labeled);
            let source = MediaItem {
                title: title.into(),
                media_type: "series".into(),
                year: Some("2025".into()),
                ..Default::default()
            };
            let mut parent = MediaItem {
                title: "Solo Leveling".into(),
                year: Some("2024".into()),
                ..source.clone()
            };
            assert_eq!(candidate_rank(&source, &parent), 2);
            parent.year = Some("2026".into());
            assert_eq!(candidate_rank(&source, &parent), 0);
            parent.year = Some("2024".into());
            parent.title = "Unrelated Show".into();
            assert_eq!(candidate_rank(&source, &parent), 0);
        }
        // Unmarked subtitles and Roman numerals inside a title keep their
        // identity; they are not proof that an older series is their parent.
        for title in ["Solo Leveling: Side Story", "Chapter II of Another Story"] {
            assert!(!series_identity(title).labeled);
        }
    }

    #[test]
    fn movie_sequels_require_full_title_or_shared_identity() {
        let mut sequel = MediaItem {
            media_type: "movie".into(),
            title: "Example II".into(),
            year: Some("2020".into()),
            ..Default::default()
        };
        let mut original = MediaItem {
            title: "Example".into(),
            year: Some("2018".into()),
            ..sequel.clone()
        };
        assert_eq!(candidate_rank(&sequel, &original), 0);
        assert_eq!(candidate_rank(&sequel, &sequel), 3);
        sequel.external_ids.imdb = Some("tt100".into());
        original.external_ids.imdb = Some("tt100".into());
        assert_eq!(candidate_rank(&sequel, &original), 3);
    }
}

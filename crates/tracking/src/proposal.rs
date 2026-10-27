//! Draft episode coverage is metadata evidence, never an active mapping.
use crate::{Assignment, ListDate, Media};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    num::NonZeroU32,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeInfo {
    pub id: String,
    pub season: Option<u32>,
    pub number: Option<u32>,
    pub title: String,
    pub released: ListDate,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReleaseRelation {
    Prequel,
    Sequel,
    Alternative,
    Other,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelatedRelease {
    pub id: NonZeroU32,
    pub relation: ReleaseRelation,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseDetails {
    pub media: Media,
    pub aliases: Vec<String>,
    pub start: ListDate,
    pub end: ListDate,
    pub relations: Vec<RelatedRelease>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedRelease {
    pub details: ReleaseDetails,
    pub season: u32,
    pub first: u32,
    pub last: u32,
    pub assignments: Vec<Assignment>,
    pub check_split: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MappingProposal {
    pub releases: Vec<ProposedRelease>,
    pub unresolved: Vec<String>,
}
/// A missing/announced format cannot establish either movie or series type.
pub fn known_anime_format(format: &str) -> bool {
    matches!(
        format.to_ascii_lowercase().as_str(),
        "tv" | "tv_short" | "ona" | "ova" | "movie" | "special" | "tv_special" | "music"
    )
}
pub fn episodic_format(format: &str) -> bool {
    matches!(
        format.to_ascii_lowercase().as_str(),
        "tv" | "tv_short" | "ona"
    )
}
fn full_date(date: ListDate) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::from_ymd_opt(
        i32::from(date.year?),
        u32::from(date.month?),
        u32::from(date.day?),
    )
}
fn compatible(episode: &EpisodeInfo, release: &ReleaseDetails) -> bool {
    let Some(day) = full_date(episode.released) else {
        return true;
    };
    if let Some(start) = full_date(release.start)
        && day.signed_duration_since(start).num_days() < -14
    {
        return false;
    }
    if let Some(end) = full_date(release.end)
        && day.signed_duration_since(end).num_days() > 14
    {
        return false;
    }
    true
}
/// Follow only an unambiguous episodic chain. Film alternatives and franchise
/// side stories must not consume rows from a TV season.
pub fn release_chain(seed: NonZeroU32, details: &[ReleaseDetails]) -> Vec<ReleaseDetails> {
    fn neighbors<'a>(
        release: &ReleaseDetails,
        relation: ReleaseRelation,
        details: &'a [ReleaseDetails],
        seen: &mut HashSet<NonZeroU32>,
    ) -> (Vec<&'a ReleaseDetails>, bool) {
        let mut results = vec![];
        let mut complete = true;
        for edge in release.relations.iter().filter(|e| e.relation == relation) {
            let Some(next) = details.iter().find(|r| r.media.id == edge.id) else {
                complete = false;
                continue;
            };
            if episodic_format(&next.media.format) {
                results.push(next);
            } else if matches!(
                next.media.format.to_ascii_lowercase().as_str(),
                "tv_special" | "special"
            ) && next.media.episodes.is_some_and(|n| n.get() == 1)
            {
                // Episode-zero specials can connect main TV parts. Follow their
                // relationships without spending an ordinary source episode.
                if !seen.insert(next.media.id) {
                    complete = false;
                    continue;
                }
                let (bridged, known) = neighbors(next, relation, details, seen);
                results.extend(bridged);
                complete &= known;
            } else if !known_anime_format(&next.media.format) {
                // An announced branch may still become another TV release;
                // do not infer a unique chain by silently discarding it.
                complete = false;
            }
        }
        results.sort_by_key(|r| r.media.id);
        results.dedup_by_key(|r| r.media.id);
        (results, complete)
    }
    let Some(mut first) = details
        .iter()
        .find(|r| r.media.id == seed && episodic_format(&r.media.format))
    else {
        return vec![];
    };
    let mut seen = HashSet::from([first.media.id]);
    loop {
        let (previous, complete) = neighbors(
            first,
            ReleaseRelation::Prequel,
            details,
            &mut HashSet::new(),
        );
        if !complete || previous.len() != 1 || !seen.insert(previous[0].media.id) {
            break;
        }
        first = previous[0];
    }
    let mut chain = vec![first.clone()];
    seen = HashSet::from([first.media.id]);
    loop {
        let (next, complete) = neighbors(
            chain.last().unwrap(),
            ReleaseRelation::Sequel,
            details,
            &mut HashSet::new(),
        );
        if !complete || next.len() != 1 || !seen.insert(next[0].media.id) {
            break;
        }
        chain.push(next[0].clone());
    }
    chain
}
/// Partition complete numbered seasons across known releases. A season's
/// drafts are published together, so an incomplete count fit cannot quietly
/// assign the wrong prefix. Air dates can anchor a partial ongoing release.
pub fn propose_coverage(episodes: &[EpisodeInfo], chain: &[ReleaseDetails]) -> MappingProposal {
    propose_coverage_at(episodes, chain, chrono::Utc::now().date_naive())
}

/// Retain a proven source-start release when a standalone season/cour has
/// franchise prequels. The caller must establish that the release identifies
/// the source's first regular episode, rather than merely a search result.
/// Counts, numbering and conflicting air dates still use the ordinary checks.
pub fn propose_coverage_from_release(
    episodes: &[EpisodeInfo],
    chain: &[ReleaseDetails],
    source_start: Option<NonZeroU32>,
) -> MappingProposal {
    let chain = match source_start {
        Some(id) => {
            let Some(index) = chain.iter().position(|release| release.media.id == id) else {
                return MappingProposal {
                    releases: vec![],
                    unresolved: episodes.iter().map(|episode| episode.id.clone()).collect(),
                };
            };
            let first_date = episodes
                .iter()
                .filter(|episode| {
                    episode.season.is_some_and(|season| season > 0)
                        && episode.number.is_some_and(|number| number > 0)
                })
                .min_by_key(|episode| (episode.season, episode.number))
                .and_then(|episode| full_date(episode.released));
            if let Some(day) = first_date {
                let matches_start = |release: &ReleaseDetails| {
                    full_date(release.start)
                        .is_some_and(|start| (day - start).num_days().abs() <= 14)
                };
                // A source ID and a dated sequel conflict. Do not let the
                // ordinary date-anchor rule silently substitute that sequel,
                // including when the selected release has no known end date.
                if !matches_start(&chain[index]) && chain.iter().any(matches_start) {
                    return MappingProposal {
                        releases: vec![],
                        unresolved: episodes.iter().map(|episode| episode.id.clone()).collect(),
                    };
                }
            }
            &chain[index..=index]
        }
        None => chain,
    };
    propose_coverage(episodes, chain)
}

pub fn propose_coverage_at(
    episodes: &[EpisodeInfo],
    chain: &[ReleaseDetails],
    today: chrono::NaiveDate,
) -> MappingProposal {
    let mut result = MappingProposal::default();
    let mut seasons = BTreeMap::<u32, Vec<&EpisodeInfo>>::new();
    let mut ids = HashSet::new();
    for episode in episodes {
        if !ids.insert(&episode.id) {
            result.unresolved = episodes.iter().map(|e| e.id.clone()).collect();
            return result;
        }
        match episode
            .season
            .filter(|s| *s > 0)
            .zip(episode.number.filter(|n| *n > 0))
        {
            Some((season, _)) => seasons.entry(season).or_default().push(episode),
            None => result.unresolved.push(episode.id.clone()),
        }
    }
    let mut release_index = 0;
    let mut blocked = false;
    for (season, mut rows) in seasons {
        rows.sort_by_key(|e| e.number);
        let contiguous = rows
            .iter()
            .enumerate()
            .all(|(i, e)| e.number == Some(i as u32 + 1));
        let mut previous_date = None;
        let chronological = rows
            .iter()
            .filter_map(|episode| full_date(episode.released))
            .all(|day| {
                let ordered = previous_date.is_none_or(|previous| previous <= day);
                previous_date = Some(day);
                ordered
            });
        if blocked || !contiguous || !chronological {
            result.unresolved.extend(rows.iter().map(|e| e.id.clone()));
            blocked = true;
            continue;
        }
        // Dates identify the first release even when the source contains only
        // a later season of a franchise. Never guess through a dated conflict.
        if let Some(day) = full_date(rows[0].released) {
            let matching: Vec<_> = chain
                .iter()
                .enumerate()
                .skip(release_index)
                .filter(|(_, r)| {
                    full_date(r.start).is_some_and(|s| (day - s).num_days().abs() <= 14)
                })
                .map(|(i, _)| i)
                .collect();
            if matching.len() == 1 {
                release_index = matching[0];
            }
        }
        let mut cursor = 0;
        let mut next_index = release_index;
        let mut drafts = vec![];
        let mut aired_prefix = false;
        let mut completed_prefix = true;
        let mut completed_end = None;
        while cursor < rows.len() {
            let Some(release) = chain.get(next_index) else {
                break;
            };
            if !known_anime_format(&release.media.format) {
                break;
            }
            let remaining = rows.len() - cursor;
            let count = release.media.episodes.map(|n| n.get() as usize);
            let anchor_dated =
                full_date(release.start).is_some() && full_date(rows[cursor].released).is_some();
            let dated = full_date(release.start).is_some()
                && rows[cursor..]
                    .iter()
                    .all(|e| full_date(e.released).is_some());
            let length = match count {
                Some(n) if n <= remaining => n,
                Some(_) if !release.media.finished && dated => remaining,
                None if !release.media.finished && anchor_dated => rows[cursor..]
                    .iter()
                    .take_while(|e| full_date(e.released).is_some_and(|day| day <= today))
                    .count(),
                _ => break,
            };
            if length == 0 {
                break;
            }
            let slice = &rows[cursor..cursor + length];
            if !slice.iter().all(|e| compatible(e, release)) {
                break;
            }
            completed_prefix &= release.media.finished
                && count == Some(length)
                && full_date(release.start)
                    .zip(full_date(release.end))
                    .is_some_and(|(start, end)| {
                        start <= end
                            && end <= today
                            && full_date(slice[0].released)
                                .is_some_and(|day| (day - start).num_days().abs() <= 14)
                            && full_date(slice[length - 1].released)
                                .is_some_and(|day| (day - end).num_days().abs() <= 14)
                    })
                && slice
                    .iter()
                    .all(|episode| full_date(episode.released).is_some_and(|day| day <= today));
            if completed_prefix {
                completed_end = full_date(release.end);
            }
            // An ongoing count-unknown release must not swallow a dated sequel.
            if count.is_none()
                && chain
                    .get(next_index + 1)
                    .and_then(|r| full_date(r.start))
                    .is_some_and(|start| {
                        slice
                            .iter()
                            .any(|e| full_date(e.released).is_some_and(|d| d >= start))
                    })
            {
                break;
            }
            let assignments = slice
                .iter()
                .enumerate()
                .map(|(i, e)| Assignment {
                    episode_id: e.id.clone(),
                    target_episode: NonZeroU32::new(i as u32 + 1).unwrap(),
                })
                .collect();
            drafts.push(ProposedRelease {
                details: release.clone(),
                season,
                first: rows[cursor].number.unwrap(),
                last: rows[cursor + length - 1].number.unwrap(),
                assignments,
                check_split: !dated,
            });
            cursor += length;
            next_index += 1;
            if count.is_none() {
                aired_prefix = true;
                break;
            }
        }
        if cursor == rows.len() {
            result.releases.extend(drafts);
            release_index = next_index;
        } else if cursor > 0
            && (aired_prefix
                || (completed_prefix
                    && completed_end
                        .zip(full_date(rows[cursor - 1].released))
                        .is_some_and(|(end, last)| {
                            let boundary = end.max(last);
                            rows[cursor..].iter().all(|episode| {
                                full_date(episode.released).is_some_and(|day| day > boundary)
                            })
                        })))
        {
            // Catalogs can append forecast episodes for an unlisted next
            // cour. Preserve a fully dated, finished release's exact coverage
            // independently of those later rows, even after forecast dates
            // pass, without assigning them or guessing the next release.
            result.releases.extend(drafts);
            result
                .unresolved
                .extend(rows[cursor..].iter().map(|e| e.id.clone()));
            blocked = true;
        } else {
            result.unresolved.extend(rows.iter().map(|e| e.id.clone()));
            blocked = true;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn n(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).unwrap()
    }
    fn release(id: u32, count: u32) -> ReleaseDetails {
        ReleaseDetails {
            media: Media {
                id: n(id),
                mal_id: Some(n(id)),
                title: format!("Part {id}"),
                format: "tv".into(),
                episodes: NonZeroU32::new(count),
                finished: true,
                year: Some(2021),
            },
            aliases: vec![],
            start: ListDate::default(),
            end: ListDate::default(),
            relations: vec![],
        }
    }
    fn rows(season: u32, count: u32) -> Vec<EpisodeInfo> {
        (1..=count)
            .map(|i| EpisodeInfo {
                id: format!("s{season}e{i}"),
                season: Some(season),
                number: Some(i),
                title: format!("Episode {i}"),
                released: ListDate::default(),
            })
            .collect()
    }
    fn relate(a: &mut ReleaseDetails, id: u32, relation: ReleaseRelation) {
        a.relations.push(RelatedRelease {
            id: n(id),
            relation,
        });
    }
    #[test]
    fn announced_unknown_format_never_consumes_numbered_source_episodes() {
        let mut known = release(199221, 13);
        let mut announced = release(218099, 12);
        announced.media.format = String::new();
        relate(&mut known, 218099, ReleaseRelation::Sequel);
        relate(&mut announced, 199221, ReleaseRelation::Prequel);
        assert_eq!(
            release_chain(n(199221), &[known.clone(), announced.clone()]),
            vec![known]
        );
        assert!(release_chain(n(218099), std::slice::from_ref(&announced)).is_empty());
        let episodes = rows(1, 12);
        let proposal = propose_coverage(&episodes, &[announced]);
        assert!(proposal.releases.is_empty());
        assert_eq!(proposal.unresolved.len(), episodes.len());
    }
    #[test]
    fn merged_season_splits_into_two_independent_releases_and_keeps_stable_ids() {
        let mut episodes = rows(1, 24);
        episodes.reverse();
        let proposal = propose_coverage(&episodes, &[release(1, 12), release(2, 12)]);
        assert!(proposal.unresolved.is_empty());
        assert_eq!(proposal.releases.len(), 2);
        assert_eq!(
            proposal.releases[1].assignments[0],
            Assignment {
                episode_id: "s1e13".into(),
                target_episode: n(1)
            }
        );
        assert_eq!(proposal.releases[1].assignments[11].target_episode, n(12));
        assert!(proposal.releases.iter().all(|r| r.check_split));
    }
    #[test]
    fn demon_slayer_merged_season_uses_seven_and_eleven_episode_tv_entries_not_movie() {
        let mut first = release(38000, 26);
        let mut train = release(49926, 7);
        let mut district = release(47778, 11);
        let mut movie = release(40456, 1);
        movie.media.format = "movie".into();
        relate(&mut first, 40456, ReleaseRelation::Sequel);
        relate(&mut first, 49926, ReleaseRelation::Sequel);
        relate(&mut train, 38000, ReleaseRelation::Prequel);
        relate(&mut train, 47778, ReleaseRelation::Sequel);
        relate(&mut district, 49926, ReleaseRelation::Prequel);
        relate(&mut district, 40456, ReleaseRelation::Prequel);
        let chain = release_chain(n(49926), &[first, train, district, movie]);
        assert_eq!(
            chain.iter().map(|r| r.media.id.get()).collect::<Vec<_>>(),
            vec![38000, 49926, 47778]
        );
        let mut episodes = rows(1, 26);
        episodes.extend(rows(2, 18));
        let proposal = propose_coverage(&episodes, &chain);
        assert!(proposal.unresolved.is_empty());
        assert_eq!(
            proposal.releases[2].assignments[0],
            Assignment {
                episode_id: "s2e8".into(),
                target_episode: n(1)
            }
        );
        assert_eq!(proposal.releases[2].assignments[10].target_episode, n(11));
    }
    #[test]
    fn mushoku_tensei_episode_zero_bridges_tv_parts_without_consuming_a_regular_episode() {
        let mut first = release(39535, 11);
        let mut second = release(45576, 12);
        let mut zero = release(55818, 1);
        zero.media.format = "tv_special".into();
        let mut season2 = release(51179, 12);
        let mut season2part2 = release(55888, 12);
        relate(&mut first, 45576, ReleaseRelation::Sequel);
        relate(&mut second, 39535, ReleaseRelation::Prequel);
        relate(&mut second, 55818, ReleaseRelation::Sequel);
        relate(&mut zero, 45576, ReleaseRelation::Prequel);
        relate(&mut zero, 51179, ReleaseRelation::Sequel);
        relate(&mut season2, 55818, ReleaseRelation::Prequel);
        relate(&mut season2, 55888, ReleaseRelation::Sequel);
        relate(&mut season2part2, 51179, ReleaseRelation::Prequel);
        let chain = release_chain(n(39535), &[first, second, zero, season2, season2part2]);
        assert_eq!(
            chain.iter().map(|r| r.media.id.get()).collect::<Vec<_>>(),
            vec![39535, 45576, 51179, 55888]
        );
        let mut episodes = rows(1, 23);
        episodes.extend(rows(2, 24));
        episodes.push(EpisodeInfo {
            id: "episode-zero".into(),
            season: Some(2),
            number: Some(0),
            title: "Fitz".into(),
            released: ListDate::default(),
        });
        let proposal = propose_coverage(&episodes, &chain);
        assert_eq!(proposal.releases.len(), 4);
        assert_eq!(proposal.unresolved, vec!["episode-zero".to_string()]);
        assert_eq!(proposal.releases[3].assignments[0].episode_id, "s2e13");
        assert_eq!(proposal.releases[3].assignments[0].target_episode, n(1));
    }
    #[test]
    fn apothecary_diaries_regular_seasons_do_not_require_upcoming_unknown_counts() {
        let mut first = release(54492, 24);
        let mut second = release(58514, 24);
        let future = release(61987, 0);
        relate(&mut first, 58514, ReleaseRelation::Sequel);
        relate(&mut second, 54492, ReleaseRelation::Prequel);
        relate(&mut second, 61987, ReleaseRelation::Sequel);
        let chain = release_chain(n(54492), &[first, second, future]);
        let mut episodes = rows(1, 24);
        episodes.extend(rows(2, 24));
        let proposal = propose_coverage(&episodes, &chain);
        assert_eq!(proposal.releases.len(), 2);
        assert!(proposal.unresolved.is_empty());
        assert_eq!(proposal.releases[1].details.media.id, n(58514));
    }
    #[test]
    fn unknown_ongoing_total_does_not_assign_forecast_episodes_or_a_future_part() {
        let mut current = release(61987, 0);
        current.media.finished = false;
        current.start = ListDate {
            year: Some(2026),
            month: Some(10),
            day: Some(2),
        };
        let mut future = release(62841, 0);
        future.media.finished = false;
        future.start = ListDate {
            year: Some(2027),
            month: Some(4),
            day: None,
        };
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 2).unwrap();
        let mut episodes = rows(3, 24);
        for (i, e) in episodes.iter_mut().enumerate() {
            let date = today + chrono::Duration::days(i as i64 * 7);
            use chrono::Datelike;
            e.released = ListDate {
                year: Some(date.year() as u16),
                month: Some(date.month() as u8),
                day: Some(date.day() as u8),
            };
        }
        let proposal = propose_coverage_at(&episodes, &[current, future], today);
        assert_eq!(proposal.releases.len(), 1);
        assert_eq!(proposal.releases[0].assignments.len(), 1);
        assert_eq!(proposal.unresolved.len(), 23);
    }
    #[test]
    fn incomplete_fit_does_not_publish_a_guessed_prefix() {
        let proposal = propose_coverage(&rows(1, 23), &[release(1, 12), release(2, 12)]);
        assert!(proposal.releases.is_empty());
        assert_eq!(proposal.unresolved.len(), 23);
    }
    #[test]
    fn numbering_gaps_duplicates_and_specials_are_not_compressed() {
        let mut episodes = rows(1, 12);
        episodes.remove(3);
        episodes.extend(rows(0, 1));
        let proposal = propose_coverage(&episodes, &[release(1, 12)]);
        assert!(proposal.releases.is_empty());
        assert_eq!(proposal.unresolved.len(), 12);
        let mut episodes = rows(1, 12);
        episodes[4].number = Some(4);
        assert!(
            propose_coverage(&episodes, &[release(1, 12)])
                .releases
                .is_empty()
        );
        let mut episodes = rows(1, 12);
        episodes[1].id = episodes[0].id.clone();
        assert!(
            propose_coverage(&episodes, &[release(1, 12)])
                .releases
                .is_empty()
        );
    }
    #[test]
    fn ambiguous_sequel_branches_and_cycles_stop_discovery() {
        let mut first = release(1, 12);
        let mut second = release(2, 12);
        let third = release(3, 12);
        relate(&mut first, 2, ReleaseRelation::Sequel);
        relate(&mut first, 3, ReleaseRelation::Sequel);
        assert_eq!(
            release_chain(n(1), &[first.clone(), second.clone(), third]).len(),
            1
        );
        first.relations.pop();
        relate(&mut second, 1, ReleaseRelation::Sequel);
        assert_eq!(release_chain(n(1), &[first, second]).len(), 2);
    }
    #[test]
    fn dated_source_can_anchor_a_later_season_but_conflicting_dates_stop_it() {
        let date = |year| ListDate {
            year: Some(year),
            month: Some(1),
            day: Some(1),
        };
        let mut first = release(1, 12);
        first.start = date(2020);
        first.end = date(2020);
        let mut second = release(2, 12);
        second.start = date(2021);
        let mut episodes = rows(2, 12);
        for e in &mut episodes {
            e.released = date(2021);
        }
        let proposal = propose_coverage(&episodes, &[first, second.clone()]);
        assert_eq!(proposal.releases[0].details.media.id, n(2));
        assert!(!proposal.releases[0].check_split);
        second.end = date(2020);
        assert!(propose_coverage(&episodes, &[second]).releases.is_empty());
    }

    #[test]
    fn proven_standalone_cour_keeps_its_release_after_prequel_discovery() {
        let mut first = release(1, 24);
        let mut second = release(2, 12);
        let mut third_part1 = release(3, 11);
        let mut third_part2 = release(4, 11);
        relate(&mut first, 2, ReleaseRelation::Sequel);
        relate(&mut second, 1, ReleaseRelation::Prequel);
        relate(&mut second, 3, ReleaseRelation::Sequel);
        relate(&mut third_part1, 2, ReleaseRelation::Prequel);
        relate(&mut third_part1, 4, ReleaseRelation::Sequel);
        relate(&mut third_part2, 3, ReleaseRelation::Prequel);
        let chain = release_chain(n(3), &[first, second, third_part1, third_part2]);
        assert_eq!(chain[0].media.id, n(1), "discovery keeps franchise context");
        let episodes = rows(1, 11);
        assert!(propose_coverage(&episodes, &chain).releases.is_empty());
        let proposal = propose_coverage_from_release(&episodes, &chain, Some(n(3)));
        assert!(proposal.unresolved.is_empty());
        assert_eq!(proposal.releases.len(), 1);
        assert_eq!(proposal.releases[0].details.media.id, n(3));
        assert_eq!(proposal.releases[0].assignments[0].episode_id, "s1e1");
        assert_eq!(proposal.releases[0].assignments[10].target_episode, n(11));
        assert!(proposal.releases[0].check_split);
        // Equal counts cannot move an already proven source release to S1.
        let mut same_counts = chain;
        same_counts[0].media.episodes = Some(n(11));
        assert_eq!(
            propose_coverage(&episodes, &same_counts).releases[0]
                .details
                .media
                .id,
            n(1)
        );
        assert_eq!(
            propose_coverage_from_release(&episodes, &same_counts, Some(n(3))).releases[0]
                .details
                .media
                .id,
            n(3)
        );
    }

    #[test]
    fn contextual_anchor_preserves_whole_series_and_existing_alignment_guards() {
        let mut chain = vec![
            release(1, 24),
            release(2, 12),
            release(3, 11),
            release(4, 11),
        ];
        let mut franchise = rows(1, 24);
        franchise.extend(rows(2, 12));
        franchise.extend(rows(3, 22));
        let full = propose_coverage_from_release(&franchise, &chain, None);
        assert!(full.unresolved.is_empty());
        assert_eq!(full.releases.len(), 4);
        assert_eq!(full.releases[3].assignments[0].episode_id, "s3e12");
        assert_eq!(full.releases[3].assignments[0].target_episode, n(1));
        let episodes = rows(3, 11);
        assert!(
            propose_coverage_from_release(&episodes, &chain, Some(n(99)))
                .releases
                .is_empty()
        );
        let mut gap = episodes.clone();
        gap[4].number = Some(9);
        assert!(
            propose_coverage_from_release(&gap, &chain, Some(n(3)))
                .releases
                .is_empty()
        );
        let mut duplicates = episodes.clone();
        duplicates[1].id = duplicates[0].id.clone();
        assert!(
            propose_coverage_from_release(&duplicates, &chain, Some(n(3)))
                .releases
                .is_empty()
        );
        chain[2].start = ListDate {
            year: Some(2023),
            month: Some(1),
            day: Some(1),
        };
        let mut conflicting = episodes;
        for episode in &mut conflicting {
            episode.released = ListDate {
                year: Some(2020),
                month: Some(1),
                day: Some(1),
            };
        }
        assert!(
            propose_coverage_from_release(&conflicting, &chain, Some(n(3)))
                .releases
                .is_empty()
        );
        // An absent end date must not let source rows dated to Part 2
        // override an otherwise proven Part 1 identity.
        chain[3].start = ListDate {
            year: Some(2024),
            month: Some(1),
            day: Some(1),
        };
        for episode in &mut conflicting {
            episode.released = chain[3].start;
        }
        assert!(
            propose_coverage_from_release(&conflicting, &chain, Some(n(3)))
                .releases
                .is_empty()
        );
        chain[2].media.episodes = None;
        chain[2].media.finished = false;
        assert!(
            propose_coverage_from_release(&rows(3, 11), &chain, Some(n(3)))
                .releases
                .is_empty()
        );
    }
    fn link_click_fixture(mal: bool) -> (Vec<EpisodeInfo>, Vec<ReleaseDetails>, chrono::NaiveDate) {
        use chrono::Datelike;
        fn date(year: i32, month: u32, day: u32) -> chrono::NaiveDate {
            chrono::NaiveDate::from_ymd_opt(year, month, day).unwrap()
        }
        fn list_date(day: chrono::NaiveDate) -> ListDate {
            ListDate {
                year: Some(day.year() as u16),
                month: Some(day.month() as u8),
                day: Some(day.day() as u8),
            }
        }
        // Reduced public metadata fetched 2026-10-27, without local Nova data:
        // AniList GraphQL ids/idMal and MAL's public 61607 details agree on
        // 11 -> 12 -> Bridon 6 -> S3 Part 1 12, with no known sequel to Part 1.
        // https://anilist.co/anime/191832/Shiguang-Dailiren-III-Part-1/
        // https://myanimelist.net/anime/61607/Shiguang_Dailiren_III
        // Cinemeta's tt14976292 has local seasons 11, 12, 6 and 24; the last
        // season's rows 13..24 forecast 2026-10-30 through 2027-01-15.
        // https://v3-cinemeta.strem.io/meta/series/tt14976292.json
        let ids = if mal {
            [44074, 49413, 56752, 61607]
        } else {
            [126403, 136484, 170166, 191832]
        };
        let starts = [
            date(2021, 4, 30),
            date(2023, 7, 14),
            date(2024, 12, 27),
            date(2026, 8, 14),
        ];
        let ends = [
            date(2021, 7, 9),
            date(2023, 9, 22),
            date(2025, 1, 31),
            date(2026, 10, 23),
        ];
        let titles = [
            "Link Click",
            "Link Click Season 2",
            "Link Click: Bridon Arc",
            "Link Click Season 3 Part 1",
        ];
        let mut chain: Vec<_> = [11, 12, 6, 12]
            .into_iter()
            .enumerate()
            .map(|(index, count)| {
                let mut details = release(ids[index], count);
                details.media.mal_id = Some(n([44074, 49413, 56752, 61607][index]));
                details.media.format = "ONA".into();
                details.media.title = titles[index].into();
                details.media.year = mal.then_some(starts[index].year() as u16);
                details.start = list_date(starts[index]);
                details.end = list_date(ends[index]);
                details
            })
            .collect();
        for index in 0..chain.len() - 1 {
            relate(&mut chain[index], ids[index + 1], ReleaseRelation::Sequel);
            relate(&mut chain[index + 1], ids[index], ReleaseRelation::Prequel);
        }
        let mut episodes = Vec::new();
        for (index, count) in [11, 12, 6, 24].into_iter().enumerate() {
            let mut season = rows(index as u32 + 1, count);
            for (episode_index, episode) in season.iter_mut().enumerate() {
                let week = if matches!(index, 1 | 3) {
                    episode_index.saturating_sub(1)
                } else {
                    episode_index
                };
                episode.id = format!("tt14976292:{}:{}", index + 1, episode_index + 1);
                episode.released = list_date(starts[index] + chrono::Duration::weeks(week as i64));
            }
            episodes.extend(season);
        }
        (episodes, chain, date(2026, 10, 27))
    }

    #[test]
    fn link_click_finished_part1_survives_full_franchise_forecast_tail_for_both_services() {
        for mal in [false, true] {
            let (episodes, details, today) = link_click_fixture(mal);
            let part1 = details[3].media.id;
            let chain = release_chain(part1, &details);
            let proposal = propose_coverage_at(&episodes, &chain, today);
            // Existing-link filtering can remove the first three releases;
            // the newly completed Part 1 still has its own complete draft.
            assert_eq!(proposal.releases.len(), 4);
            assert_eq!(proposal.releases[3].details.media.id, part1);
            assert_eq!(proposal.releases[3].assignments.len(), 12);
            assert_eq!(
                proposal.releases[3].assignments[0].episode_id,
                "tt14976292:4:1"
            );
            assert_eq!(proposal.releases[3].assignments[11].target_episode, n(12));
            assert!(!proposal.releases[3].check_split);
            assert_eq!(
                proposal.unresolved,
                (13..=24)
                    .map(|episode| format!("tt14976292:4:{episode}"))
                    .collect::<Vec<_>>()
            );
            for later in [
                chrono::NaiveDate::from_ymd_opt(2026, 10, 30).unwrap(),
                chrono::NaiveDate::from_ymd_opt(2027, 2, 1).unwrap(),
            ] {
                assert_eq!(
                    propose_coverage_at(&episodes, &chain, later),
                    proposal,
                    "passing unresolved forecast dates cannot erase proven Part 1 coverage"
                );
            }
        }
    }

    #[test]
    fn finished_prefix_requires_known_counts_complete_aired_dates_and_a_later_chronological_tail() {
        let (episodes, chain, today) = link_click_fixture(false);
        let prefix = |rows: &[EpisodeInfo], releases: &[ReleaseDetails], at| {
            propose_coverage_at(rows, releases, at)
                .releases
                .iter()
                .any(|release| release.details.media.id == n(191832))
        };
        let final_air_day = chrono::NaiveDate::from_ymd_opt(2026, 10, 23).unwrap();
        assert!(
            prefix(&episodes, &chain, final_air_day),
            "completed source rows may air today"
        );
        assert!(
            !prefix(
                &episodes,
                &chain,
                chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap()
            ),
            "a future completion date cannot prove a finished source prefix"
        );
        for missing_prefix_date in [false, true] {
            let mut incomplete = episodes.clone();
            let index = incomplete
                .iter()
                .position(|episode| {
                    episode.id
                        == if missing_prefix_date {
                            "tt14976292:4:1"
                        } else {
                            "tt14976292:4:13"
                        }
                })
                .unwrap();
            incomplete[index].released.day = None;
            assert!(!prefix(&incomplete, &chain, today));
        }
        for unknown in [false, true] {
            let mut incomplete = chain.clone();
            if unknown {
                incomplete[3].media.episodes = None;
            } else {
                incomplete[3].end.day = None;
            }
            assert!(!prefix(&episodes, &incomplete, today));
        }
        let mut ongoing = chain.clone();
        ongoing[3].media.finished = false;
        assert!(!prefix(&episodes, &ongoing, today));
        let mut gap = episodes.clone();
        gap.iter_mut()
            .find(|episode| episode.id == "tt14976292:4:13")
            .unwrap()
            .number = Some(14);
        assert!(!prefix(&gap, &chain, today));
        let mut conflict = episodes.clone();
        conflict
            .iter_mut()
            .find(|episode| episode.id == "tt14976292:4:12")
            .unwrap()
            .released
            .month = Some(12);
        assert!(!prefix(&conflict, &chain, today));
        let mut overlaps_official_end = episodes.clone();
        overlaps_official_end
            .iter_mut()
            .find(|episode| episode.id == "tt14976292:4:12")
            .unwrap()
            .released
            .day = Some(16);
        overlaps_official_end
            .iter_mut()
            .find(|episode| episode.id == "tt14976292:4:13")
            .unwrap()
            .released
            .day = Some(23);
        assert!(
            !prefix(&overlaps_official_end, &chain, today),
            "the tail must also follow the official end, even when source dates differ within tolerance"
        );
        let mut same_day_tail = episodes.clone();
        same_day_tail
            .iter_mut()
            .find(|episode| episode.id == "tt14976292:4:13")
            .unwrap()
            .released
            .day = Some(23);
        assert!(
            !prefix(&same_day_tail, &chain, today),
            "a tail on the prefix boundary is ambiguous"
        );
        let mut reversed_tail = episodes.clone();
        reversed_tail
            .iter_mut()
            .find(|episode| episode.id == "tt14976292:4:14")
            .unwrap()
            .released = ListDate {
            year: Some(2026),
            month: Some(10),
            day: Some(29),
        };
        assert!(
            !prefix(&reversed_tail, &chain, today),
            "unresolved tail chronology still must be consistent"
        );
        let mut reversed_prefix = episodes;
        reversed_prefix
            .iter_mut()
            .find(|episode| episode.id == "tt14976292:4:5")
            .unwrap()
            .released = ListDate {
            year: Some(2026),
            month: Some(8),
            day: Some(15),
        };
        assert!(
            !prefix(&reversed_prefix, &chain, today),
            "prefix dates must follow episode numbering"
        );
    }

    #[test]
    fn ongoing_unknown_counts_need_dates_and_never_swallow_a_sequel() {
        let mut ongoing = release(1, 0);
        ongoing.media.finished = false;
        ongoing.start = ListDate {
            year: Some(2021),
            month: Some(1),
            day: Some(1),
        };
        let mut episodes = rows(1, 6);
        assert!(
            propose_coverage(&episodes, &[ongoing.clone()])
                .releases
                .is_empty()
        );
        for e in &mut episodes {
            e.released = ongoing.start;
        }
        assert_eq!(
            propose_coverage(&episodes, &[ongoing.clone()]).releases[0]
                .assignments
                .len(),
            6
        );
        let mut next = release(2, 6);
        next.start = ongoing.start;
        assert!(
            propose_coverage(&episodes, &[ongoing, next])
                .releases
                .is_empty()
        );
    }
}
